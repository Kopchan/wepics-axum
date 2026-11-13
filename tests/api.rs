use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header};
use chrono::{DateTime, TimeZone, Utc};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tempfile::TempDir;
use tokio_stream::StreamExt;
use tower::ServiceExt;

use wepics_axum::controllers::{AlbumInfo, AlbumPage, MediaPage};
use wepics_axum::models::{Album, Media, MediaType, ROOT_ALBUM_HASH, natural_sort_key};
use wepics_axum::routes;
use wepics_axum::state::AppState;

/// Temporary directory under `target/`.
///
/// The system temp directory is not always writable for the test binary (some
/// Windows setups deny it), while `target/` always is.
fn workspace_tempdir() -> TempDir {
    let base = Path::new("target").join("tmp");
    std::fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new()
        .prefix("test-")
        .tempdir_in(&base)
        .unwrap()
}

async fn setup_raw() -> (TempDir, AppState) {
    let dir = workspace_tempdir();

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
///
/// The root album is marked freshly indexed so unrelated tests do not kick off
/// background syncs; sync-specific tests reset `indexed_at` explicitly.
async fn setup() -> (TempDir, AppState) {
    let (dir, state) = setup_raw().await;
    state.initialize().await.unwrap();

    sqlx::query("UPDATE albums SET indexed_at = ?1 WHERE hash = ?2")
        .bind(Utc::now())
        .bind(ROOT_ALBUM_HASH)
        .execute(&state.db)
        .await
        .unwrap();

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
        "INSERT INTO albums (name, hash, sort_name, path, indexed_at)
         VALUES (?1, ?2, ?3, ?2, ?4)
         RETURNING id, name, hash, parent_id, path, indexed_at, sort_name, created_at, updated_at",
    )
    .bind(name)
    .bind(hash)
    .bind(natural_sort_key(name))
    .bind(Utc::now())
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
        "INSERT INTO albums (name, hash, sort_name, parent_id, path, created_at, indexed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         RETURNING id, name, hash, parent_id, path, indexed_at, sort_name, created_at, updated_at",
    )
    .bind(name)
    .bind(format!("{parent_id}-{name}"))
    .bind(natural_sort_key(name))
    .bind(parent_id)
    .bind(format!("test/{parent_id}/{name}"))
    .bind(created_at)
    .bind(Utc::now())
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

fn parse_album_info(body: &[u8]) -> AlbumInfo {
    serde_json::from_slice(body).unwrap_or_else(|err| {
        panic!(
            "invalid AlbumInfo json ({err}): {}",
            String::from_utf8_lossy(body)
        )
    })
}

fn album_names(page: &AlbumPage) -> Vec<&str> {
    page.items.iter().map(|album| album.name.as_str()).collect()
}

async fn root_album_id(state: &AppState) -> i64 {
    sqlx::query_scalar("SELECT id FROM albums WHERE hash = ?1")
        .bind(ROOT_ALBUM_HASH)
        .fetch_one(&state.db)
        .await
        .unwrap()
}

async fn set_indexed_at(state: &AppState, album_id: i64, value: Option<DateTime<Utc>>) {
    sqlx::query("UPDATE albums SET indexed_at = ?1 WHERE id = ?2")
        .bind(value)
        .bind(album_id)
        .execute(&state.db)
        .await
        .unwrap();
}

/// Waits until a background sync has written `indexed_at` after `after`.
async fn wait_for_indexed(state: &AppState, album_id: i64, after: DateTime<Utc>) {
    for _ in 0..300 {
        let indexed: Option<DateTime<Utc>> =
            sqlx::query_scalar("SELECT indexed_at FROM albums WHERE id = ?1")
                .bind(album_id)
                .fetch_one(&state.db)
                .await
                .unwrap();

        if let Some(indexed) = indexed
            && indexed > after
        {
            return;
        }

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    panic!("album {album_id} was not indexed after {after}");
}

async fn wait_for_media_count(state: &AppState, album_path: &str, expected: i64) {
    let prefix = format!("{album_path}/%");

    for _ in 0..300 {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM media m JOIN albums a ON a.id = m.album_id \
             WHERE a.path = ?1 OR a.path LIKE ?2",
        )
        .bind(album_path)
        .bind(&prefix)
        .fetch_one(&state.db)
        .await
        .unwrap();

        if count == expected {
            return;
        }

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    panic!("expected {expected} media under `{album_path}`");
}

#[cfg(windows)]
fn create_dir_link(link: &Path, target: &Path) -> bool {
    std::process::Command::new("cmd")
        .args([
            "/C",
            "mklink",
            "/J",
            link.to_str().unwrap(),
            target.to_str().unwrap(),
        ])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(unix)]
fn create_dir_link(link: &Path, target: &Path) -> bool {
    std::os::unix::fs::symlink(target, link).is_ok()
}

#[tokio::test]
async fn root_album_is_created_at_startup() {
    let (dir, state) = setup().await;
    let media_dir = state.media_dir.clone();
    let pool = state.db.clone();
    let app = app(state);

    assert!(media_dir.is_dir(), "media dir must exist after startup");

    let (status, _, body) = send(&app, "/api/albums/root").await;
    assert_eq!(status, StatusCode::OK);

    let info = parse_album_info(&body);
    assert_eq!(info.album.hash, ROOT_ALBUM_HASH);
    assert_eq!(info.album.name, "root");
    let album = info.album;

    // Second request reuses the row instead of creating a duplicate.
    let (status, _, body) = send(&app, "/api/albums/root").await;
    assert_eq!(status, StatusCode::OK);
    let again = parse_album_info(&body).album;
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

    let (status, _, _) = send(&app, "/api/albums/root").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let albums: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM albums")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(albums, 0, "requests must not create albums");
    assert!(!state.media_dir.exists());

    // Startup is the only place that bootstraps storage.
    state.initialize().await.unwrap();
    let (status, _, _) = send(&app, "/api/albums/root").await;
    assert_eq!(status, StatusCode::OK);

    drop(dir);
}

#[tokio::test]
async fn album_by_hash_and_unknown_hash() {
    let (dir, state) = setup().await;
    let album = insert_album(&state.db, "Trip", "abc123").await;
    let app = app(state);

    let (status, _, body) = send(&app, "/api/albums/abc123").await;
    assert_eq!(status, StatusCode::OK);
    let fetched = parse_album_info(&body).album;

    // `sort_name` and `path` are internal: clients never see them.
    assert_eq!(fetched.sort_name, "");
    assert_eq!(fetched.path, "");
    assert_eq!(fetched.id, album.id);
    assert_eq!(fetched.name, album.name);
    assert_eq!(fetched.hash, album.hash);
    assert_eq!(fetched.parent_id, album.parent_id);
    assert_eq!(fetched.created_at, album.created_at);

    let (status, _, _) = send(&app, "/api/albums/nope").await;
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

    let (status, _, body) = send(&app, "/api/albums/abc123/media?page=2&limit=2").await;
    assert_eq!(status, StatusCode::OK);
    let page = parse_page(&body);
    assert_eq!(page.total, 5);
    assert_eq!(page.pages, 3);
    assert_eq!(page.page, 2);
    assert_eq!(page.limit, 2);
    let names: Vec<_> = page.items.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, vec!["m3.jpg", "m4.jpg"]);

    // Default limit is 30.
    let (_, _, body) = send(&app, "/api/albums/abc123/media").await;
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

    let (_, _, body) = send(&app, "/api/albums/abc123/media?sort=name").await;
    let names: Vec<_> = parse_page(&body)
        .items
        .into_iter()
        .map(|m| m.name)
        .collect();
    assert_eq!(names, vec!["img1.jpg", "img2.jpg", "img10.jpg"]);

    let (_, _, body) = send(&app, "/api/albums/abc123/media?sort=-name").await;
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

    let (_, _, body) = send(&app, "/api/albums/abc123/media?sort=-size").await;
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
        "/api/albums/abc123/media?page=0",
        "/api/albums/abc123/media?limit=0",
        "/api/albums/abc123/media?limit=101",
        "/api/albums/abc123/media?sort=width",
        "/api/albums/abc123/media?sort=--name",
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
    std::fs::write(album_dir.join(&media.name), b"jpegdata").unwrap();

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

    let (status, _, body) = send(&app, "/api/albums/root").await;
    assert_eq!(status, StatusCode::OK);
    let root = parse_album_info(&body).album;

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
    std::fs::write(dir.path().join("media").join(&media.name), b"png").unwrap();

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
    std::fs::write(album_dir.join(&media.name), b"jpegdata").unwrap();

    let app = app(state);

    // Full response advertises ranges, validators and immutable caching.
    let (status, headers, body) = send(&app, "/files/abc123/h1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::ACCEPT_RANGES], "bytes");
    assert_eq!(
        headers[header::CACHE_CONTROL],
        "public, max-age=31536000, immutable"
    );
    assert!(headers.contains_key(header::ETAG));
    assert!(headers.contains_key(header::LAST_MODIFIED));
    assert_eq!(body, b"jpegdata");

    // A matching validator is answered with 304 by `ServeFile`.
    let mut conditional = HeaderMap::new();
    conditional.insert(header::IF_NONE_MATCH, headers[header::ETAG].clone());
    let (status, _, body) = send_with(&app, Method::GET, "/files/abc123/h1", conditional).await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);
    assert!(body.is_empty());

    // Multi-range requests are ignored and served as a normal full response.
    let (status, headers, body) = send_range(&app, "/files/abc123/h1", "bytes=0-1,3-4").await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get(header::CONTENT_RANGE).is_none());
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
    assert!(exposed.contains("etag"), "exposed: {exposed}");
    assert!(exposed.contains("cache-control"), "exposed: {exposed}");

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
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
    let methods = headers[header::ACCESS_CONTROL_ALLOW_METHODS]
        .to_str()
        .unwrap();
    assert!(methods.contains("GET"), "methods: {methods}");
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

#[tokio::test]
async fn album_info_is_wrapped_and_syncs_only_when_stale() {
    let (dir, state) = setup().await;
    let root_id = root_album_id(&state).await;
    let app = app(state.clone());

    // Fresh album: wrapped in `album`, no sync notice.
    let (status, _, body) = send(&app, "/api/albums/root").await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json.get("hash").is_none(), "album must be wrapped");
    assert!(json.get("sync").is_none(), "fresh album must not sync");
    assert_eq!(json["album"]["hash"], ROOT_ALBUM_HASH);

    // 30 minutes old is still fresh.
    set_indexed_at(
        &state,
        root_id,
        Some(Utc::now() - chrono::Duration::minutes(30)),
    )
    .await;
    let (_, _, body) = send(&app, "/api/albums/root").await;
    assert!(parse_album_info(&body).sync.is_none());

    // Older than an hour starts exactly one background task.
    let stale = Utc::now() - chrono::Duration::minutes(61);
    set_indexed_at(&state, root_id, Some(stale)).await;
    let (_, _, body) = send(&app, "/api/albums/root").await;
    assert_eq!(parse_album_info(&body).sync.as_deref(), Some("started"));

    wait_for_indexed(&state, root_id, stale).await;

    // Once done the index is fresh again: no second task.
    let (_, _, body) = send(&app, "/api/albums/root").await;
    assert!(parse_album_info(&body).sync.is_none());

    drop(dir);
}

#[tokio::test]
async fn sync_indexes_files_and_discovers_child_albums() {
    let (dir, state) = setup().await;
    let root_id = root_album_id(&state).await;
    let media_dir = state.media_dir.clone();

    std::fs::write(media_dir.join("photo10.jpg"), b"0123456789").unwrap();
    std::fs::write(media_dir.join("photo2.png"), b"png").unwrap();
    std::fs::write(media_dir.join("dup.jpg"), b"dup-jpg").unwrap();
    std::fs::write(media_dir.join("dup.png"), b"dup-png").unwrap();
    std::fs::write(media_dir.join("clip.mp4"), b"mp4").unwrap();
    std::fs::write(media_dir.join("song.mp3"), b"mp3").unwrap();
    std::fs::write(media_dir.join("notes.txt"), b"ignored").unwrap();
    std::fs::write(media_dir.join(".hidden.jpg"), b"hidden").unwrap();

    std::fs::create_dir(media_dir.join("Winter")).unwrap();
    std::fs::write(media_dir.join("Winter").join("snow.jpg"), b"snow").unwrap();

    set_indexed_at(&state, root_id, None).await;
    let app = app(state.clone());
    let since = Utc::now() - chrono::Duration::seconds(1);

    let (_, _, body) = send(&app, "/api/albums/root").await;
    assert_eq!(parse_album_info(&body).sync.as_deref(), Some("started"));
    wait_for_indexed(&state, root_id, since).await;

    let rows: Vec<Media> = sqlx::query_as(
        "SELECT id, name, hash, \"type\", album_id, width, height, duration_sec, date, size,
                sort_name, created_at, updated_at
         FROM media WHERE album_id = ?1 ORDER BY name",
    )
    .bind(root_id)
    .fetch_all(&state.db)
    .await
    .unwrap();

    let names: Vec<&str> = rows.iter().map(|media| media.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "clip.mp4",
            "dup.jpg",
            "dup.png",
            "photo10.jpg",
            "photo2.png",
            "song.mp3"
        ]
    );
    assert!(
        !names.contains(&"snow.jpg"),
        "subfolders are albums, not media"
    );

    let photo = rows
        .iter()
        .find(|media| media.name == "photo10.jpg")
        .unwrap();
    assert_eq!(photo.hash.len(), 24, "media hash is generated by SQLite");
    assert_eq!(photo.r#type, MediaType::Image);
    assert_eq!(photo.size, 10);
    assert_eq!(photo.sort_name, "photo0000000010.jpg");
    assert!(photo.date.is_some());
    assert_eq!(photo.width, 0);
    assert_eq!(photo.height, 0);
    assert_eq!(photo.duration_sec, None);

    assert_eq!(
        rows.iter()
            .find(|media| media.name == "clip.mp4")
            .unwrap()
            .r#type,
        MediaType::Video
    );
    assert_eq!(
        rows.iter()
            .find(|media| media.name == "song.mp3")
            .unwrap()
            .r#type,
        MediaType::Audio
    );
    assert_eq!(
        rows.iter()
            .find(|media| media.name == "photo2.png")
            .unwrap()
            .r#type,
        MediaType::Image
    );

    // The subdirectory became a child album with its own (not yet indexed) row.
    let winter: Album = sqlx::query_as(
        "SELECT id, name, hash, parent_id, path, indexed_at, sort_name, created_at, updated_at
         FROM albums WHERE path = 'Winter'",
    )
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert_eq!(winter.name, "Winter");
    assert_eq!(winter.parent_id, Some(root_id));
    assert_eq!(winter.path, "Winter");
    assert_eq!(winter.indexed_at, None);
    assert_eq!(winter.hash.len(), 24, "hash is generated by SQLite");

    let (_, _, body) = send(&app, "/api/albums/root/child").await;
    assert_eq!(album_names(&parse_album_page(&body)), vec!["Winter"]);

    // Requesting the child indexes its own folder on demand.
    let winter_since = Utc::now() - chrono::Duration::seconds(1);
    let (_, _, body) = send(&app, &format!("/api/albums/{}", winter.hash)).await;
    assert_eq!(parse_album_info(&body).sync.as_deref(), Some("started"));
    wait_for_indexed(&state, winter.id, winter_since).await;

    let child_media: Vec<Media> = sqlx::query_as(
        "SELECT id, name, hash, \"type\", album_id, width, height, duration_sec, date, size,
                sort_name, created_at, updated_at
         FROM media WHERE album_id = ?1",
    )
    .bind(winter.id)
    .fetch_all(&state.db)
    .await
    .unwrap();
    assert_eq!(child_media.len(), 1);
    assert_eq!(child_media[0].name, "snow.jpg");
    assert_eq!(child_media[0].hash.len(), 24);

    let (status, _, body) = send(
        &app,
        &format!("/files/{}/{}", winter.hash, child_media[0].hash),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"snow");

    let (status, _, body) = send(&app, &format!("/files/root/{}", photo.hash)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"0123456789");

    drop(dir);
}

#[tokio::test]
async fn resync_removes_media_that_disappeared() {
    let (dir, state) = setup().await;
    let root_id = root_album_id(&state).await;
    let media_dir = state.media_dir.clone();

    std::fs::write(media_dir.join("keep.jpg"), b"keep").unwrap();
    std::fs::write(media_dir.join("drop.jpg"), b"drop").unwrap();

    set_indexed_at(&state, root_id, None).await;
    let app = app(state.clone());
    let since = Utc::now() - chrono::Duration::seconds(1);
    let _ = send(&app, "/api/albums/root").await;
    wait_for_indexed(&state, root_id, since).await;

    std::fs::remove_file(media_dir.join("drop.jpg")).unwrap();
    std::fs::write(media_dir.join("new.png"), b"new").unwrap();

    let stale = Utc::now() - chrono::Duration::hours(2);
    set_indexed_at(&state, root_id, Some(stale)).await;
    let (_, _, body) = send(&app, "/api/albums/root").await;
    assert_eq!(parse_album_info(&body).sync.as_deref(), Some("started"));
    wait_for_indexed(&state, root_id, stale).await;

    let names: Vec<String> =
        sqlx::query_scalar("SELECT name FROM media WHERE album_id = ?1 ORDER BY name")
            .bind(root_id)
            .fetch_all(&state.db)
            .await
            .unwrap();
    assert_eq!(names, vec!["keep.jpg", "new.png"]);

    drop(dir);
}
#[tokio::test]
async fn root_and_album_pages_are_spa_for_people_and_og_for_bots() {
    let (dir, state) = setup().await;
    let album = insert_album(&state.db, "Trip & Fun", "trip").await;
    insert_media(
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

    let app = app(state);

    let (status, headers, body) = send(&app, "/trip").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/html")
    );
    let html = String::from_utf8_lossy(&body);
    assert!(html.contains("id=\"app\""), "SPA shell expected");

    let (status, _, body) = send(&app, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&body).contains("id=\"app\""));

    let mut bot = HeaderMap::new();
    bot.insert(
        header::USER_AGENT,
        HeaderValue::from_static("Twitterbot/1.0"),
    );
    let (status, _, body) = send_with(&app, Method::GET, "/trip", bot.clone()).await;
    assert_eq!(status, StatusCode::OK);
    let html = String::from_utf8_lossy(&body);
    assert!(html.contains("og:title"), "OG title missing: {html}");
    assert!(
        html.contains("Trip &amp; Fun"),
        "album name must be escaped"
    );
    assert!(html.contains("/files/trip/h1"), "OG image missing");
    assert!(html.contains("og:image:width"));

    let (status, _, body) = send_with(&app, Method::GET, "/", bot).await;
    assert_eq!(status, StatusCode::OK);
    let html = String::from_utf8_lossy(&body);
    assert!(html.contains("og:site_name"));
    assert!(html.contains("og:title"));

    let mut bot = HeaderMap::new();
    bot.insert(
        header::USER_AGENT,
        HeaderValue::from_static("Googlebot/2.1"),
    );
    let (status, _, body) = send_with(&app, Method::GET, "/missing", bot).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(String::from_utf8_lossy(&body).contains("404"));

    drop(dir);
}

#[tokio::test]
async fn static_frontend_files_are_served() {
    let (dir, state) = setup().await;
    let app = app(state);

    // Vite emits content-hashed asset names, so discover them from the built
    // entry point instead of hardcoding names that change on every build.
    let index = std::fs::read_to_string("public/index.html").expect("built frontend is embedded");
    let asset_path = |marker: &str| -> String {
        let start = index
            .find(marker)
            .unwrap_or_else(|| panic!("index.html has no {marker:?}"));
        let rest = &index[start..];
        let value = rest
            .find("=\"./")
            .unwrap_or_else(|| panic!("index.html has no URL after {marker:?}"));
        let rest = &rest[value + 4..];
        let end = rest.find('"').expect("unterminated asset reference");
        format!("/public/{}", &rest[..end])
    };
    let js_path = asset_path("<script type=\"module\"");
    let css_path = asset_path("rel=\"stylesheet\"");
    let manifest_path = asset_path("rel=\"manifest\"");

    for (path, needle, content_type) in [
        (js_path.as_str(), "mediaHash", "javascript"),
        (css_path.as_str(), "--accent", "css"),
        (manifest_path.as_str(), "\"wepics\"", "json"),
        ("/public/icon/circle.svg", "<svg", "svg"),
        ("/public/icon/circle16.png", "", "image/png"),
    ] {
        let (status, headers, body) = send(&app, path).await;
        assert_eq!(status, StatusCode::OK, "path: {path}");
        let got_type = headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap_or_default()
            .to_ascii_lowercase();
        assert!(
            got_type.contains(content_type),
            "path: {path}, content-type: {got_type}"
        );
        // Assets carry no content hash, so a stale cached copy would keep
        // running old JavaScript after an update.
        assert_eq!(
            headers[header::CACHE_CONTROL].to_str().unwrap_or_default(),
            "no-cache",
            "path: {path}"
        );
        if !needle.is_empty() {
            assert!(
                String::from_utf8_lossy(&body).contains(needle),
                "path: {path}"
            );
        }
    }

    let (status, headers, _) = send(&app, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers[header::CACHE_CONTROL].to_str().unwrap_or_default(),
        "no-cache"
    );

    // API and file namespaces must keep returning JSON/plain 404, not the SPA.
    let (status, headers, _) = send(&app, "/api/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap_or_default()
            .contains("json")
    );

    drop(dir);
}
#[tokio::test]
async fn manual_index_honours_force_and_bare_flags() {
    let (dir, state) = setup().await;
    let album = insert_album(&state.db, "Trip", "trip").await;
    let album_dir = state.media_dir.join("trip");
    std::fs::create_dir_all(&album_dir).unwrap();
    std::fs::write(album_dir.join("a.jpg"), b"a").unwrap();
    set_indexed_at(&state, album.id, Some(Utc::now())).await;

    let app = app(state.clone());

    let (status, _, body) = send(&app, "/api/albums/trip/index").await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["sync"], "fresh");

    let since = Utc::now() - chrono::Duration::seconds(1);
    let (status, _, body) = send(&app, "/api/albums/trip/index?force&nested").await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["sync"], "started");

    wait_for_indexed(&state, album.id, since).await;
    wait_for_media_count(&state, "trip", 1).await;

    drop(dir);
}

#[tokio::test]
async fn nested_index_walks_descendants() {
    let (dir, state) = setup().await;
    let root = insert_album(&state.db, "Trip", "trip").await;
    let root_dir = state.media_dir.join("trip");
    let winter_dir = root_dir.join("winter");
    let deep_dir = winter_dir.join("deep");
    std::fs::create_dir_all(&deep_dir).unwrap();
    std::fs::write(root_dir.join("a.jpg"), b"a").unwrap();
    std::fs::write(winter_dir.join("b.jpg"), b"b").unwrap();
    std::fs::write(deep_dir.join("c.jpg"), b"c").unwrap();

    let app = app(state.clone());
    let (status, _, body) = send(&app, "/api/albums/trip/index?force&nested").await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["sync"], "started");

    wait_for_media_count(&state, "trip", 3).await;

    let rows: Vec<(String, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT path, indexed_at FROM albums WHERE path IN ('trip/winter', 'trip/winter/deep') ORDER BY path",
    )
    .fetch_all(&state.db)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2, "nested albums must exist");
    assert_eq!(rows[0].0, "trip/winter");
    assert!(rows[0].1.is_some());
    assert_eq!(rows[1].0, "trip/winter/deep");
    assert!(rows[1].1.is_some());

    wait_for_indexed(&state, root.id, Utc::now() - chrono::Duration::seconds(1)).await;

    drop(dir);
}

#[tokio::test]
async fn index_sse_reports_progress_and_completion() {
    let (dir, state) = setup().await;
    let album = insert_album(&state.db, "Trip", "trip").await;
    let album_dir = state.media_dir.join("trip");
    std::fs::create_dir_all(&album_dir).unwrap();
    std::fs::write(album_dir.join("a.jpg"), b"a").unwrap();

    let app = app(state.clone());

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/albums/trip/index/events")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );

    let mut body = response.into_body().into_data_stream();

    let (status, _, _) = send(&app, "/api/albums/trip/index?force").await;
    assert_eq!(status, StatusCode::OK);

    let mut text = String::new();
    for _ in 0..50 {
        let next = tokio::time::timeout(Duration::from_secs(3), body.next()).await;
        let Ok(Some(Ok(chunk))) = next else {
            break;
        };
        text.push_str(&String::from_utf8_lossy(&chunk));
        if text.contains("\"done\":true") {
            break;
        }
    }

    assert!(text.contains("\"done\":true"), "SSE output: {text}");
    assert!(text.contains("\"new_media\":1"), "SSE output: {text}");
    assert!(text.contains("\"files_total\":1"), "SSE output: {text}");

    wait_for_indexed(&state, album.id, Utc::now() - chrono::Duration::seconds(1)).await;

    drop(dir);
}
#[tokio::test]
async fn symlinked_album_directory_is_indexed() {
    let (dir, state) = setup().await;
    let target = dir.path().join("linked-album");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("photo.jpg"), b"x").unwrap();

    let link = state.media_dir.join("Moe");
    if !create_dir_link(&link, &target) {
        eprintln!("skipping symlink test: cannot create directory link");
        return;
    }

    let app = app(state.clone());
    let root_id = root_album_id(&state).await;
    set_indexed_at(&state, root_id, None).await;
    let since = Utc::now() - chrono::Duration::seconds(1);
    let (status, _, _) = send(&app, "/api/albums/root/index?force").await;
    assert_eq!(status, StatusCode::OK);
    wait_for_indexed(&state, root_id, since).await;

    let moe: Album = sqlx::query_as(
        "SELECT id, name, hash, parent_id, path, indexed_at, sort_name, created_at, updated_at \
         FROM albums WHERE path = 'Moe'",
    )
    .fetch_one(&state.db)
    .await
    .unwrap();

    let since = Utc::now() - chrono::Duration::seconds(1);
    let (status, _, _) = send(&app, &format!("/api/albums/{}/index?force", moe.hash)).await;
    assert_eq!(status, StatusCode::OK);
    wait_for_indexed(&state, moe.id, since).await;
    wait_for_media_count(&state, "Moe", 1).await;

    let media_hash: String = sqlx::query_scalar("SELECT hash FROM media WHERE album_id = ?1")
        .bind(moe.id)
        .fetch_one(&state.db)
        .await
        .unwrap();
    let (status, _, body) = send(&app, &format!("/files/{}/{}", moe.hash, media_hash)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"x");

    #[cfg(windows)]
    std::fs::remove_dir(&link).ok();
    #[cfg(unix)]
    std::fs::remove_file(&link).ok();

    drop(dir);
}

fn parse_v1(body: &[u8]) -> serde_json::Value {
    serde_json::from_slice(body)
        .unwrap_or_else(|err| panic!("invalid v1 json ({err}): {}", String::from_utf8_lossy(body)))
}

fn v1_names(value: &serde_json::Value, key: &str) -> Vec<String> {
    value[key]
        .as_array()
        .unwrap_or_else(|| panic!("`{key}` is not an array: {value}"))
        .iter()
        .map(|item| item["name"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn v1_album_returns_stats_previews_and_signed_links() {
    let (_dir, state) = setup().await;
    let root_id = root_album_id(&state).await;

    for index in 1..=6 {
        insert_media(
            &state.db,
            root_id,
            NewMedia {
                name: &format!("photo{index}.jpg"),
                hash: &format!("m{index}"),
                date: Some(at(index)),
                size: i64::from(index) * 10,
            },
        )
        .await;
    }

    let app = app(state.clone());
    let (status, _, body) = send(&app, "/api/v1/albums/root").await;
    assert_eq!(status, StatusCode::OK);

    let album = parse_v1(&body);
    assert_eq!(album["hash"], "root");
    assert_eq!(album["mediasCount"], 6);
    assert_eq!(album["imagesCount"], 6);
    assert_eq!(album["videosCount"], 0);
    assert_eq!(album["albumsCount"], 0);
    assert!(
        album.get("sync").is_none(),
        "fresh albums have no sync state"
    );

    // Four previews by default, naturally sorted by name.
    let images = album["images"].as_array().unwrap();
    assert_eq!(images.len(), 4);
    assert_eq!(
        v1_names(&album, "images"),
        vec!["photo1.jpg", "photo2.jpg", "photo3.jpg", "photo4.jpg"]
    );

    for image in images {
        assert_eq!(image["type"], "image");
        assert_eq!(image["width"], 800);
        assert_eq!(image["height"], 600);
        assert_eq!(image["album"]["hash"], "root");
        assert_eq!(image["album"]["name"], "root");

        let media_hash = image["mediaHash"].as_str().unwrap();
        let token = image["hash"].as_str().unwrap();
        let exp = image["exp"].as_i64().unwrap();
        assert!(
            state.verify_media("root", media_hash, token, exp),
            "previews carry a valid secure link"
        );
        assert!(exp > Utc::now().timestamp());
    }

    // `mlimit=0` disables previews entirely.
    let (status, _, body) = send(&app, "/api/v1/albums/root?mlimit=0").await;
    assert_eq!(status, StatusCode::OK);
    assert!(parse_v1(&body).get("images").is_none());

    // A reverse-prefixed sort array is honoured.
    let (status, _, body) = send(&app, "/api/v1/albums/root?msort=-size&mlimit=2").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        v1_names(&parse_v1(&body), "images"),
        vec!["photo6.jpg", "photo5.jpg"]
    );

    // Type filtering reaches the preview query too.
    let (status, _, body) = send(&app, "/api/v1/albums/root?mtype=vid").await;
    assert_eq!(status, StatusCode::OK);
    assert!(parse_v1(&body)["images"].as_array().unwrap().is_empty());

    let (status, _, _) = send(&app, "/api/v1/albums/root?mtype=bogus").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn v1_album_layers_children_sorting_and_ancestors() {
    let (_dir, state) = setup().await;
    let root_id = root_album_id(&state).await;

    let bravo = insert_child(&state.db, root_id, "Bravo", at(2)).await;
    let alpha = insert_child(&state.db, root_id, "Alpha", at(1)).await;
    let grand = insert_child(&state.db, alpha.id, "Grand", at(1)).await;

    for (album_id, prefix) in [
        (root_id, "root"),
        (bravo.id, "bravo"),
        (alpha.id, "alpha"),
        (grand.id, "grand"),
    ] {
        for index in 1..=3 {
            insert_media(
                &state.db,
                album_id,
                NewMedia {
                    name: &format!("{prefix}{index}.jpg"),
                    hash: &format!("{prefix}-m{index}"),
                    date: Some(at(index)),
                    size: 10,
                },
            )
            .await;
        }
    }

    let app = app(state.clone());

    // Default: one layer of children, four previews everywhere.
    let (status, _, body) = send(&app, "/api/v1/albums/root").await;
    assert_eq!(status, StatusCode::OK);
    let album = parse_v1(&body);
    assert_eq!(v1_names(&album, "children"), vec!["Alpha", "Bravo"]);
    assert_eq!(album["children"][0]["images"].as_array().unwrap().len(), 3);
    assert!(
        album["children"][0].get("children").is_none(),
        "grandchildren need `layers=2`"
    );

    // `asort` reverses child order.
    let (status, _, body) = send(&app, "/api/v1/albums/root?asort=-name").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        v1_names(&parse_v1(&body), "children"),
        vec!["Bravo", "Alpha"]
    );

    // `layers=0` returns only the requested album.
    let (status, _, body) = send(&app, "/api/v1/albums/root?layers=0").await;
    assert_eq!(status, StatusCode::OK);
    assert!(parse_v1(&body).get("children").is_none());

    // `layers=2` adds grandchildren; `mlimitnest` limits their previews.
    let (status, _, body) = send(&app, "/api/v1/albums/root?layers=2&mlimitnest=1&mlimit=2").await;
    assert_eq!(status, StatusCode::OK);
    let album = parse_v1(&body);
    assert_eq!(album["images"].as_array().unwrap().len(), 2);
    let alpha = &album["children"][0];
    assert_eq!(alpha["name"], "Alpha");
    assert_eq!(alpha["images"].as_array().unwrap().len(), 1);
    let grand_preview = &alpha["children"][0];
    assert_eq!(grand_preview["name"], "Grand");
    assert_eq!(grand_preview["images"].as_array().unwrap().len(), 1);

    // Ancestors are root-first.
    let (status, _, body) = send(&app, &format!("/api/v1/albums/{}", grand.hash)).await;
    assert_eq!(status, StatusCode::OK);
    let album = parse_v1(&body);
    assert_eq!(
        v1_names(&album, "ancestors"),
        vec!["root", "Alpha"],
        "ancestors must be ordered from root down"
    );
}

#[tokio::test]
async fn v1_media_paginates_sorts_and_filters() {
    let (_dir, state) = setup().await;
    let root_id = root_album_id(&state).await;

    for index in 1..=5 {
        insert_media(
            &state.db,
            root_id,
            NewMedia {
                name: &format!("photo{index}.jpg"),
                hash: &format!("m{index}"),
                date: Some(at(index)),
                size: i64::from(index) * 10,
            },
        )
        .await;
    }
    sqlx::query(
        r#"INSERT INTO media (name, hash, "type", album_id, width, height, date, size, sort_name)
           VALUES ('clip.mp4', 'v1', 'video', ?1, 0, 0, NULL, 100, ?2)"#,
    )
    .bind(root_id)
    .bind(natural_sort_key("clip.mp4"))
    .execute(&state.db)
    .await
    .unwrap();

    let app = app(state.clone());

    let (status, _, body) = send(
        &app,
        "/api/v1/album/root/media?limit=2&page=2&sort=date&type=img",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let page = parse_v1(&body);
    assert_eq!(page["page"], 2);
    assert_eq!(page["per_page"], 2);
    assert_eq!(page["total"], 5);
    assert_eq!(page["pages"], 3);
    assert_eq!(
        v1_names(&page, "pictures"),
        vec!["photo3.jpg", "photo4.jpg"]
    );

    let (status, _, body) = send(&app, "/api/v1/album/root/media?sort=-size").await;
    assert_eq!(status, StatusCode::OK);
    let page = parse_v1(&body);
    assert_eq!(
        v1_names(&page, "pictures"),
        vec![
            "clip.mp4",
            "photo5.jpg",
            "photo4.jpg",
            "photo3.jpg",
            "photo2.jpg",
            "photo1.jpg"
        ]
    );

    let picture = &page["pictures"][0];
    let media_hash = picture["mediaHash"].as_str().unwrap();
    let token = picture["hash"].as_str().unwrap();
    let exp = picture["exp"].as_i64().unwrap();
    assert!(state.verify_media("root", media_hash, token, exp));

    let (status, _, _) = send(&app, "/api/v1/album/root/media?type=bogus").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _, _) = send(&app, "/api/v1/album/missing/media").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn v1_preview_and_original_require_a_valid_signature() {
    let (_dir, state) = setup().await;
    let root_id = root_album_id(&state).await;

    let source = state.media_dir.join("photo.png");
    image::RgbImage::from_pixel(800, 600, image::Rgb([10, 20, 30]))
        .save(&source)
        .unwrap();
    let original = std::fs::read(&source).unwrap();

    insert_media(
        &state.db,
        root_id,
        NewMedia {
            name: "photo.png",
            hash: "media1",
            date: Some(at(1)),
            size: original.len() as i64,
        },
    )
    .await;

    let app = app(state.clone());
    let exp = Utc::now().timestamp() + 3600;
    let token = state.sign_media("root", "media1", exp);
    let link = format!("hash={token}&exp={exp}");

    // Preview: 800x600 scaled down to width 144 -> 108 px high, WebP encoded.
    let (status, headers, body) = send(&app, &format!("/thumb/v1/root/media1/w144?{link}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "image/webp");
    assert_eq!(&body[..4], b"RIFF");
    assert_eq!(&body[8..12], b"WEBP");
    let preview = image::load_from_memory(&body).unwrap();
    assert_eq!((preview.width(), preview.height()), (144, 108));
    assert!(
        state.thumb_dir().join("w144").join("media1.webp").is_file(),
        "generated previews are cached"
    );

    // Original: exact bytes, inline disposition and the Laravel cache policy.
    let (status, headers, body) = send(&app, &format!("/file/v1/root/media1?{link}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, original);
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

    // Missing, tampered and expired signatures are rejected.
    let (status, _, _) = send(&app, "/thumb/v1/root/media1/w144").await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _, _) = send(&app, &format!("/file/v1/root/media1?hash=bad&exp={exp}")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let expired_at = Utc::now().timestamp() - 10;
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
}

#[tokio::test]
async fn v1_indexing_stores_image_dimensions() {
    let (_dir, state) = setup().await;
    let root_id = root_album_id(&state).await;

    image::RgbImage::from_pixel(40, 20, image::Rgb([1, 2, 3]))
        .save(state.media_dir.join("tiny.png"))
        .unwrap();

    set_indexed_at(&state, root_id, None).await;
    let app = app(state.clone());
    let since = Utc::now() - chrono::Duration::seconds(1);
    let (status, _, _) = send(&app, "/api/albums/root/index?force").await;
    assert_eq!(status, StatusCode::OK);
    wait_for_indexed(&state, root_id, since).await;

    let (width, height): (i64, i64) =
        sqlx::query_as("SELECT width, height FROM media WHERE album_id = ?1")
            .bind(root_id)
            .fetch_one(&state.db)
            .await
            .unwrap();
    assert_eq!((width, height), (40, 20));

    let (status, _, body) = send(&app, "/api/v1/albums/root").await;
    assert_eq!(status, StatusCode::OK);
    let album = parse_v1(&body);
    assert_eq!(album["images"][0]["name"], "tiny.png");
    assert_eq!(album["images"][0]["width"], 40);
    assert_eq!(album["images"][0]["height"], 20);
}
