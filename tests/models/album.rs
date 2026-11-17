//! Album rows, constraints and cascade behaviour.

use sqlx::SqlitePool;

use crate::common::{insert_typed_media, test_pool};
use wepics_axum::models::{Album, MediaType, natural_sort_key};

async fn insert_album(pool: &SqlitePool, name: &str, hash: &str, parent_id: Option<i64>) -> Album {
    sqlx::query_as(
        "INSERT INTO albums (name, hash, sort_name, parent_id, path) VALUES (?1, ?2, ?3, ?4, ?2)
         RETURNING id, name, hash, parent_id, path, alias, old_aliases, rating_level, order_level, guest_allow, indexed_at, sort_name, created_at, updated_at",
    )
    .bind(name)
    .bind(hash)
    .bind(natural_sort_key(name))
    .bind(parent_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn album_rows_roundtrip_with_metadata() {
    let pool = test_pool().await;

    let root = insert_album(&pool, "Photos", "root-hash", None).await;
    assert_eq!(root.parent_id, None);
    assert_eq!(root.indexed_at, None);
    assert_eq!(root.path, "root-hash");
    assert_eq!(root.sort_name, "photos");

    let child = insert_album(&pool, "2026", "child-hash", Some(root.id)).await;
    assert_eq!(child.parent_id, Some(root.id));

    let now = chrono::Utc::now();
    let age_sec = (now - root.created_at).num_seconds();
    assert!(
        (0..60).contains(&age_sec),
        "created_at looks wrong: {age_sec}s"
    );
}

#[tokio::test]
async fn deleting_album_cascades_to_media() {
    let pool = test_pool().await;
    let album = insert_album(&pool, "Temp", "h", None).await;

    insert_typed_media(
        &pool,
        album.id,
        "a.png",
        "h1",
        MediaType::Image,
        10,
        10,
        None,
        0,
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
        "SELECT id, name, hash, parent_id, path, alias, old_aliases, rating_level, order_level, guest_allow, indexed_at, sort_name, created_at, updated_at
         FROM albums WHERE id = ?1",
    )
    .bind(album.id)
    .fetch_one(&pool)
    .await
    .unwrap();

    assert!(updated.updated_at > album.updated_at);
    assert_eq!(updated.created_at, album.created_at);
}
