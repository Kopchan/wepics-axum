//! Shared helpers for integration tests.
//!
//! Every top-level test crate pulls this module in with `mod common;`. Some
//! helpers are only used by one crate, hence the crate-wide `dead_code` allow.

#![allow(dead_code)]

use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header};
use chrono::{DateTime, TimeZone, Utc};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tempfile::TempDir;
use tower::ServiceExt;

use wepics_axum::config::AppConfig;
use wepics_axum::models::{Album, Media, MediaType, ROOT_ALBUM_HASH, natural_sort_key};
use wepics_axum::routes;
use wepics_axum::state::AppState;

/// Temporary directory under `target/`.
///
/// The system temp directory is not always writable for the test binary (some
/// Windows setups deny it), while `target/` always is.
pub fn workspace_tempdir() -> TempDir {
    let base = Path::new("target").join("tmp");
    std::fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new()
        .prefix("test-")
        .tempdir_in(&base)
        .unwrap()
}

/// In-memory database with all migrations applied.
pub async fn test_pool() -> SqlitePool {
    let options = SqliteConnectOptions::from_str("sqlite::memory:")
        .unwrap()
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    pool
}

pub async fn setup_raw() -> (TempDir, AppState) {
    let dir = workspace_tempdir();
    let pool = test_pool().await;

    let mut config = AppConfig::defaults();
    config.media_dir = dir.path().join("media");
    config.cache_dir = dir.path().join("cache");

    let state = AppState::new(pool, Arc::new(config));
    (dir, state)
}

/// Mirrors what `AppState::connect` does at startup.
///
/// The root album is marked freshly indexed so unrelated tests do not kick off
/// background syncs; sync-specific tests reset `indexed_at` explicitly.
pub async fn setup() -> (TempDir, AppState) {
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

pub fn app(state: AppState) -> Router {
    routes::app(state)
}

pub async fn send(app: &Router, uri: &str) -> (StatusCode, HeaderMap, Vec<u8>) {
    send_with(app, Method::GET, uri, HeaderMap::new()).await
}

pub async fn send_range(app: &Router, uri: &str, range: &str) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::RANGE,
        HeaderValue::from_str(range).expect("range value is ASCII"),
    );

    send_with(app, Method::GET, uri, headers).await
}

pub async fn send_with(
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

pub async fn insert_album(pool: &SqlitePool, name: &str, hash: &str) -> Album {
    sqlx::query_as(
        "INSERT INTO albums (name, hash, sort_name, path, indexed_at)
         VALUES (?1, ?2, ?3, ?2, ?4)
         RETURNING id, name, hash, parent_id, path, alias, old_aliases, rating_level, order_level, guest_allow, indexed_at, sort_name, created_at, updated_at",
    )
    .bind(name)
    .bind(hash)
    .bind(natural_sort_key(name))
    .bind(Utc::now())
    .fetch_one(pool)
    .await
    .unwrap()
}

pub async fn insert_child(
    pool: &SqlitePool,
    parent_id: i64,
    name: &str,
    created_at: DateTime<Utc>,
) -> Album {
    sqlx::query_as(
        "INSERT INTO albums (name, hash, sort_name, parent_id, path, created_at, indexed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         RETURNING id, name, hash, parent_id, path, alias, old_aliases, rating_level, order_level, guest_allow, indexed_at, sort_name, created_at, updated_at",
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

pub struct NewMedia<'a> {
    pub name: &'a str,
    pub hash: &'a str,
    pub date: Option<DateTime<Utc>>,
    pub size: i64,
}

/// Inserts an 800x600 still image.
pub async fn insert_media(pool: &SqlitePool, album_id: i64, media: NewMedia<'_>) -> Media {
    insert_typed_media(
        pool,
        album_id,
        media.name,
        media.hash,
        MediaType::Image,
        800,
        600,
        media.date,
        media.size,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn insert_typed_media(
    pool: &SqlitePool,
    album_id: i64,
    name: &str,
    hash: &str,
    media_type: MediaType,
    width: i64,
    height: i64,
    date: Option<DateTime<Utc>>,
    size: i64,
) -> Media {
    sqlx::query_as(
        r#"INSERT INTO media (name, hash, "type", album_id, width, height, date, size, sort_name)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
           RETURNING id, name, hash, "type", album_id, width, height, duration_sec,
                     date, size, rating_level, sort_name, created_at, updated_at"#,
    )
    .bind(name)
    .bind(hash)
    .bind(media_type)
    .bind(album_id)
    .bind(width)
    .bind(height)
    .bind(date)
    .bind(size)
    .bind(natural_sort_key(name))
    .fetch_one(pool)
    .await
    .unwrap()
}

pub fn at(day: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, day, 12, 0, 0).unwrap()
}

pub fn parse_v1(body: &[u8]) -> serde_json::Value {
    serde_json::from_slice(body)
        .unwrap_or_else(|err| panic!("invalid v1 json ({err}): {}", String::from_utf8_lossy(body)))
}

pub fn v1_names(value: &serde_json::Value, key: &str) -> Vec<String> {
    value[key]
        .as_array()
        .unwrap_or_else(|| panic!("`{key}` is not an array: {value}"))
        .iter()
        .map(|item| {
            let name = item["name"].as_str().unwrap();
            match item["ext"].as_str() {
                Some(ext) if !ext.is_empty() => format!("{name}.{ext}"),
                _ => name.to_string(),
            }
        })
        .collect()
}

pub async fn root_album_id(state: &AppState) -> i64 {
    sqlx::query_scalar("SELECT id FROM albums WHERE hash = ?1")
        .bind(ROOT_ALBUM_HASH)
        .fetch_one(&state.db)
        .await
        .unwrap()
}

pub async fn set_indexed_at(state: &AppState, album_id: i64, value: Option<DateTime<Utc>>) {
    sqlx::query("UPDATE albums SET indexed_at = ?1 WHERE id = ?2")
        .bind(value)
        .bind(album_id)
        .execute(&state.db)
        .await
        .unwrap();
}

/// Waits until a background sync has written `indexed_at` after `after`.
pub async fn wait_for_indexed(state: &AppState, album_id: i64, after: DateTime<Utc>) {
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

pub async fn wait_for_media_count(state: &AppState, album_path: &str, expected: i64) {
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

/// Query string with a fresh secure link for one media file.
pub fn link(state: &AppState, album_hash: &str, media_hash: &str) -> String {
    let exp = Utc::now().timestamp() + 3600;
    let token = state.sign_media(album_hash, media_hash, exp);
    format!("sign={token}&exp={exp}")
}

/// Writes a solid-colour PNG through the `image` crate (test-only dependency).
pub fn write_png(path: &Path, width: u32, height: u32) {
    image::RgbImage::from_pixel(width, height, image::Rgb([10, 20, 30]))
        .save(path)
        .unwrap();
}

#[cfg(windows)]
pub fn create_dir_link(link: &Path, target: &Path) -> bool {
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
pub fn create_dir_link(link: &Path, target: &Path) -> bool {
    std::os::unix::fs::symlink(target, link).is_ok()
}
