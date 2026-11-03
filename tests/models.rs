use std::str::FromStr;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use wepics_axum::models::{Album, Media, MediaType, natural_sort_key};

/// In-memory DB; `max_connections(1)` keeps every query on the same database.
async fn test_pool() -> SqlitePool {
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

async fn insert_album(pool: &SqlitePool, name: &str, hash: &str, parent_id: Option<i64>) -> Album {
    sqlx::query_as(
        "INSERT INTO albums (name, hash, sort_name, parent_id) VALUES (?1, ?2, ?3, ?4)
         RETURNING id, name, hash, parent_id, indexed_at, sort_name, created_at, updated_at",
    )
    .bind(name)
    .bind(hash)
    .bind(natural_sort_key(name))
    .bind(parent_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[allow(clippy::too_many_arguments)]
async fn insert_media(
    pool: &SqlitePool,
    name: &str,
    hash: &str,
    media_type: MediaType,
    album_id: i64,
    width: i64,
    height: i64,
    duration_sec: Option<f64>,
) -> Media {
    sqlx::query_as(
        r#"INSERT INTO media (name, hash, "type", album_id, width, height, duration_sec)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
           RETURNING id, name, hash, "type", album_id, width, height, duration_sec,
                     date, size, sort_name, created_at, updated_at"#,
    )
    .bind(name)
    .bind(hash)
    .bind(media_type)
    .bind(album_id)
    .bind(width)
    .bind(height)
    .bind(duration_sec)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn album_and_media_roundtrip() {
    let pool = test_pool().await;

    let root = insert_album(&pool, "Photos", "root-hash", None).await;
    assert_eq!(root.parent_id, None);
    assert_eq!(root.indexed_at, None);
    assert_eq!(root.sort_name, "photos");

    let child = insert_album(&pool, "2026", "child-hash", Some(root.id)).await;
    assert_eq!(child.parent_id, Some(root.id));

    let photo = insert_media(
        &pool,
        "cat.jpg",
        "h1",
        MediaType::Image,
        root.id,
        1920,
        1080,
        None,
    )
    .await;
    assert_eq!(photo.r#type, MediaType::Image);
    assert_eq!(photo.width, 1920);
    assert_eq!(photo.height, 1080);
    assert_eq!(photo.duration_sec, None);
    assert_eq!(photo.date, None);
    assert_eq!(photo.size, 0);
    assert_eq!(photo.sort_name, "");

    let clip = insert_media(
        &pool,
        "clip.mp4",
        "h2",
        MediaType::Video,
        root.id,
        1280,
        720,
        Some(12.5),
    )
    .await;
    assert_eq!(clip.r#type, MediaType::Video);
    assert_eq!(clip.duration_sec, Some(12.5));

    // Timestamps come from SQLite defaults and deserialize into DateTime<Utc>.
    let now = chrono::Utc::now();
    let age_sec = (now - photo.created_at).num_seconds();
    assert!(
        (0..60).contains(&age_sec),
        "created_at looks wrong: {age_sec}s"
    );
}

#[tokio::test]
async fn deleting_album_cascades_to_media() {
    let pool = test_pool().await;

    let album = insert_album(&pool, "Temp", "h", None).await;
    insert_media(
        &pool,
        "a.png",
        "h1",
        MediaType::Image,
        album.id,
        10,
        10,
        None,
    )
    .await;

    sqlx::query("DELETE FROM albums WHERE id = ?1")
        .bind(album.id)
        .execute(&pool)
        .await
        .unwrap();

    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(left, 0, "media must be deleted together with its album");
}

#[tokio::test]
async fn album_hashes_are_unique() {
    let pool = test_pool().await;

    insert_album(&pool, "A", "same-hash", None).await;
    let err = sqlx::query("INSERT INTO albums (name, hash) VALUES ('B', 'same-hash')")
        .execute(&pool)
        .await
        .unwrap_err();

    assert!(
        err.to_string().contains("UNIQUE"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn media_hash_is_unique_per_album_only() {
    let pool = test_pool().await;

    let first = insert_album(&pool, "A", "album-a", None).await;
    let second = insert_album(&pool, "B", "album-b", None).await;

    insert_media(
        &pool,
        "x.png",
        "same",
        MediaType::Image,
        first.id,
        1,
        1,
        None,
    )
    .await;
    // Same hash in another album is fine.
    insert_media(
        &pool,
        "x.png",
        "same",
        MediaType::Image,
        second.id,
        1,
        1,
        None,
    )
    .await;

    let err = sqlx::query(
        r#"INSERT INTO media (name, hash, "type", album_id, width, height)
           VALUES ('x.png', 'same', 'image', ?1, 1, 1)"#,
    )
    .bind(first.id)
    .execute(&pool)
    .await
    .unwrap_err();

    assert!(
        err.to_string().contains("UNIQUE"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn invalid_media_type_is_rejected_by_schema() {
    let pool = test_pool().await;
    let album = insert_album(&pool, "A", "h", None).await;

    let err = sqlx::query(
        r#"INSERT INTO media (name, hash, "type", album_id, width, height)
           VALUES ('x.bin', 'h1', 'document', ?1, 0, 0)"#,
    )
    .bind(album.id)
    .execute(&pool)
    .await
    .unwrap_err();

    assert!(err.to_string().contains("CHECK"), "unexpected error: {err}");
}

#[tokio::test]
async fn media_requires_an_existing_album() {
    let pool = test_pool().await;

    let err = sqlx::query(
        r#"INSERT INTO media (name, hash, "type", album_id, width, height)
           VALUES ('x.png', 'h1', 'image', 999, 1, 1)"#,
    )
    .execute(&pool)
    .await
    .unwrap_err();

    assert!(
        err.to_string().contains("FOREIGN KEY"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn updated_at_is_refreshed_on_update() {
    let pool = test_pool().await;
    let album = insert_album(&pool, "A", "h", None).await;

    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    sqlx::query("UPDATE albums SET name = ?1 WHERE id = ?2")
        .bind("B")
        .bind(album.id)
        .execute(&pool)
        .await
        .unwrap();

    let updated: Album = sqlx::query_as(
        "SELECT id, name, hash, parent_id, indexed_at, sort_name, created_at, updated_at
         FROM albums WHERE id = ?1",
    )
    .bind(album.id)
    .fetch_one(&pool)
    .await
    .unwrap();

    assert!(updated.updated_at > album.updated_at);
    assert_eq!(updated.created_at, album.created_at);
}
