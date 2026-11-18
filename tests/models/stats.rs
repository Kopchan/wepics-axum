//! `album_stats` triggers and nested counter derivation.
//!
//! Direct counters are materialized by SQLite triggers; nested counters are
//! summed with a recursive album walk. These tests pin both behaviours down
//! without going through the HTTP layer.

use sqlx::SqlitePool;

use wepics_axum::controllers::v1::types::load_album_stats_map;
use wepics_axum::models::MediaType;

use crate::common::{at, insert_album, insert_child, insert_typed_media, test_pool};

#[derive(Debug, Default, PartialEq, sqlx::FromRow)]
struct Counters {
    medias_count: i64,
    images_count: i64,
    videos_count: i64,
    audios_count: i64,
    duplica_count: i64,
    albums_count: i64,
    size: i64,
    duration: f64,
}

/// Direct counters stored for one album, or `None` when the row is missing.
async fn counters(pool: &SqlitePool, album_id: i64) -> Option<Counters> {
    sqlx::query_as(
        "SELECT medias_count, images_count, videos_count, audios_count, \
                duplica_count, albums_count, size, duration \
         FROM album_stats WHERE album_id = ?1",
    )
    .bind(album_id)
    .fetch_optional(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn album_triggers_create_and_remove_stats_rows() {
    let pool = test_pool().await;

    let root = insert_album(&pool, "Photos", "root-hash").await;
    assert_eq!(
        counters(&pool, root.id).await,
        Some(Counters::default()),
        "every new album starts with an empty stats row"
    );

    let child = insert_child(&pool, root.id, "2026", at(1)).await;
    assert_eq!(counters(&pool, child.id).await, Some(Counters::default()));
    assert_eq!(
        counters(&pool, root.id).await.unwrap().albums_count,
        1,
        "a new child album is counted in its parent"
    );

    // Re-parenting keeps both sides in sync.
    let other = insert_album(&pool, "Other", "other-hash").await;
    sqlx::query("UPDATE albums SET parent_id = ?1 WHERE id = ?2")
        .bind(other.id)
        .bind(child.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(counters(&pool, root.id).await.unwrap().albums_count, 0);
    assert_eq!(counters(&pool, other.id).await.unwrap().albums_count, 1);

    // Deleting an album drops its stats row and decrements the parent.
    sqlx::query("DELETE FROM albums WHERE id = ?1")
        .bind(child.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(counters(&pool, child.id).await, None);
    assert_eq!(counters(&pool, other.id).await.unwrap().albums_count, 0);

    // Cascading deletes clean up the whole subtree.
    let grandchild = insert_child(&pool, other.id, "Nested", at(2)).await;
    sqlx::query("DELETE FROM albums WHERE id = ?1")
        .bind(other.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(counters(&pool, other.id).await, None);
    assert_eq!(counters(&pool, grandchild.id).await, None);

    drop(pool);
}

#[tokio::test]
async fn media_triggers_keep_counters_in_sync() {
    let pool = test_pool().await;
    let album = insert_album(&pool, "Album", "album-hash").await;

    insert_typed_media(
        &pool,
        album.id,
        "still.jpg",
        "m-image",
        MediaType::Image,
        10,
        10,
        None,
        100,
    )
    .await;
    insert_typed_media(
        &pool,
        album.id,
        "logo.svg",
        "m-vector",
        MediaType::Vector,
        10,
        10,
        None,
        50,
    )
    .await;
    let video = insert_typed_media(
        &pool,
        album.id,
        "clip.mp4",
        "m-video",
        MediaType::Video,
        10,
        10,
        None,
        400,
    )
    .await;
    insert_typed_media(
        &pool,
        album.id,
        "loop.gif",
        "m-animated",
        MediaType::Animated,
        10,
        10,
        None,
        70,
    )
    .await;
    insert_typed_media(
        &pool,
        album.id,
        "track.flac",
        "m-audio",
        MediaType::Audio,
        0,
        0,
        None,
        900,
    )
    .await;

    let after_insert = counters(&pool, album.id).await.unwrap();
    assert_eq!(
        after_insert,
        Counters {
            medias_count: 5,
            images_count: 2,
            videos_count: 2,
            audios_count: 1,
            duplica_count: 0,
            albums_count: 0,
            size: 1520,
            duration: 0.0,
        },
        "vectors count as images, animated files as videos"
    );

    // A meaningful update moves the deltas, including across albums.
    let other = insert_album(&pool, "Other", "other-hash").await;
    sqlx::query("UPDATE media SET album_id = ?1, duration_sec = 12.5 WHERE id = ?2")
        .bind(other.id)
        .bind(video.id)
        .execute(&pool)
        .await
        .unwrap();

    assert_eq!(
        counters(&pool, album.id).await.unwrap(),
        Counters {
            medias_count: 4,
            images_count: 2,
            videos_count: 1,
            audios_count: 1,
            duplica_count: 0,
            albums_count: 0,
            size: 1120,
            duration: 0.0,
        }
    );
    assert_eq!(
        counters(&pool, other.id).await.unwrap(),
        Counters {
            medias_count: 1,
            images_count: 0,
            videos_count: 1,
            audios_count: 0,
            duplica_count: 0,
            albums_count: 0,
            size: 400,
            duration: 12.5,
        }
    );

    // Deleting subtracts the row again.
    sqlx::query("DELETE FROM media WHERE id = ?1")
        .bind(video.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        counters(&pool, other.id).await.unwrap(),
        Counters::default()
    );
    assert_eq!(counters(&pool, album.id).await.unwrap().medias_count, 4);

    drop(pool);
}

#[tokio::test]
async fn stats_updates_do_not_touch_album_timestamps() {
    let pool = test_pool().await;
    let album = insert_album(&pool, "Album", "album-hash").await;

    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    insert_typed_media(
        &pool,
        album.id,
        "still.jpg",
        "m-image",
        MediaType::Image,
        10,
        10,
        None,
        100,
    )
    .await;

    let updated_at: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT updated_at FROM albums WHERE id = ?1")
            .bind(album.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        updated_at, album.updated_at,
        "media inserts must not bump albums.updated_at"
    );

    drop(pool);
}

#[tokio::test]
async fn nested_stats_sum_the_subtree_without_the_root() {
    let pool = test_pool().await;

    let root = insert_album(&pool, "Root", "root-hash").await;
    let alpha = insert_child(&pool, root.id, "Alpha", at(1)).await;
    let bravo = insert_child(&pool, alpha.id, "Bravo", at(2)).await;

    insert_typed_media(
        &pool,
        root.id,
        "root.jpg",
        "root-m",
        MediaType::Image,
        10,
        10,
        None,
        7,
    )
    .await;
    insert_typed_media(
        &pool,
        alpha.id,
        "alpha.jpg",
        "alpha-m",
        MediaType::Image,
        10,
        10,
        None,
        10,
    )
    .await;
    insert_typed_media(
        &pool,
        bravo.id,
        "bravo.jpg",
        "bravo-m",
        MediaType::Image,
        10,
        10,
        None,
        20,
    )
    .await;
    insert_typed_media(
        &pool,
        bravo.id,
        "bravo.flac",
        "bravo-a",
        MediaType::Audio,
        0,
        0,
        None,
        30,
    )
    .await;
    sqlx::query("UPDATE media SET duration_sec = 5.5 WHERE hash = 'bravo-a'")
        .execute(&pool)
        .await
        .unwrap();

    // A copy in the deepest album also shows up in every ancestor's nested count.
    let bravo_media_id: i64 = sqlx::query_scalar("SELECT id FROM media WHERE hash = 'bravo-m'")
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO duplica (name, media_id, album_id, data) \
         VALUES ('bravo-copy.jpg', ?1, ?2, '{}')",
    )
    .bind(bravo_media_id)
    .bind(bravo.id)
    .execute(&pool)
    .await
    .unwrap();

    let stats = load_album_stats_map(&pool, &[root.id, alpha.id, bravo.id])
        .await
        .unwrap();
    assert_eq!(stats.len(), 3);

    let root_stats = stats[&root.id];
    assert_eq!(root_stats.direct.medias_count, 1);
    assert_eq!(root_stats.direct.size, 7);
    assert_eq!(
        root_stats.nested,
        wepics_axum::controllers::v1::types::AlbumStats {
            medias_count: 3,
            images_count: 2,
            videos_count: 0,
            audios_count: 1,
            duplica_count: 1,
            albums_count: 2,
            size: 60,
            duration: 5.5,
        },
        "nested stats cover every descendant but not the album itself"
    );

    let alpha_stats = stats[&alpha.id];
    assert_eq!(alpha_stats.direct.medias_count, 1);
    assert_eq!(alpha_stats.nested.medias_count, 2);
    assert_eq!(alpha_stats.nested.duplica_count, 1);
    assert_eq!(alpha_stats.nested.albums_count, 1);
    assert_eq!(alpha_stats.nested.size, 50);
    assert_eq!(alpha_stats.nested.duration, 5.5);

    let bravo_stats = stats[&bravo.id];
    assert_eq!(bravo_stats.direct.medias_count, 2);
    assert_eq!(bravo_stats.direct.duplica_count, 1);
    assert_eq!(bravo_stats.nested.duplica_count, 0);
    assert_eq!(
        bravo_stats.nested,
        wepics_axum::controllers::v1::types::AlbumStats::default(),
        "a leaf album has no nested counters"
    );

    drop(pool);
}

#[tokio::test]
async fn duplica_triggers_keep_the_counter_in_sync() {
    let pool = test_pool().await;
    let album = insert_album(&pool, "Album", "album-hash").await;
    let owner = insert_typed_media(
        &pool,
        album.id,
        "photo.jpg",
        "m-photo",
        MediaType::Image,
        10,
        10,
        None,
        100,
    )
    .await;

    sqlx::query(
        "INSERT INTO duplica (name, media_id, album_id, data) VALUES ('copy.jpg', ?1, ?2, '{}')",
    )
    .bind(owner.id)
    .bind(album.id)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(counters(&pool, album.id).await.unwrap().duplica_count, 1);

    // A rename keeps the count unchanged.
    sqlx::query("UPDATE duplica SET name = 'renamed.jpg' WHERE album_id = ?1")
        .bind(album.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(counters(&pool, album.id).await.unwrap().duplica_count, 1);

    // Deleting the copy subtracts it.
    sqlx::query("DELETE FROM duplica WHERE album_id = ?1")
        .bind(album.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(counters(&pool, album.id).await.unwrap().duplica_count, 0);

    // Deleting the owner cascades the remaining copies and their counter.
    sqlx::query(
        "INSERT INTO duplica (name, media_id, album_id, data) VALUES ('copy.jpg', ?1, ?2, '{}')",
    )
    .bind(owner.id)
    .bind(album.id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("DELETE FROM media WHERE id = ?1")
        .bind(owner.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(counters(&pool, album.id).await.unwrap().duplica_count, 0);

    drop(pool);
}
