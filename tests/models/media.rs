//! Media rows, constraints and natural sorting.

use crate::common::{insert_album, insert_typed_media, test_pool};
use wepics_axum::models::{MediaType, natural_sort_key};

#[tokio::test]
async fn media_rows_roundtrip_with_metadata() {
    let pool = test_pool().await;
    let album = insert_album(&pool, "Photos", "album-hash").await;

    let photo = insert_typed_media(
        &pool,
        album.id,
        "cat.jpg",
        "h1",
        MediaType::Image,
        1920,
        1080,
        None,
        0,
    )
    .await;
    assert_eq!(photo.r#type, MediaType::Image);
    assert_eq!(photo.width, 1920);
    assert_eq!(photo.height, 1080);
    assert_eq!(photo.duration_sec, None);
    assert_eq!(photo.date, None);
    assert_eq!(photo.size, 0);
    assert_eq!(photo.sort_name, "cat.jpg");

    let clip = insert_typed_media(
        &pool,
        album.id,
        "clip.mp4",
        "h2",
        MediaType::Video,
        1280,
        720,
        None,
        0,
    )
    .await;
    assert_eq!(clip.r#type, MediaType::Video);

    let logo = insert_typed_media(
        &pool,
        album.id,
        "logo.svg",
        "h3",
        MediaType::Vector,
        64,
        32,
        None,
        0,
    )
    .await;
    assert_eq!(logo.r#type, MediaType::Vector);
}

#[tokio::test]
async fn media_hashes_are_unique_per_album_only() {
    let pool = test_pool().await;

    let first = insert_album(&pool, "A", "album-a").await;
    let second = insert_album(&pool, "B", "album-b").await;

    insert_typed_media(
        &pool,
        first.id,
        "x.png",
        "same",
        MediaType::Image,
        1,
        1,
        None,
        0,
    )
    .await;
    // Same hash in another album is fine.
    insert_typed_media(
        &pool,
        second.id,
        "x.png",
        "same",
        MediaType::Image,
        1,
        1,
        None,
        0,
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
    let album = insert_album(&pool, "A", "h").await;

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

#[test]
fn natural_sort_key_pads_numbers_and_lowercases() {
    assert_eq!(natural_sort_key("Img 2.JPG"), "img 0000000002.jpg");
    assert_eq!(natural_sort_key("img0007"), "img0000000007");
    assert_eq!(natural_sort_key("no digits"), "no digits");
}

#[test]
fn natural_sort_key_orders_names_naturally() {
    let mut names = vec!["img10.jpg", "img2.jpg", "img1.jpg", "IMG20.jpg"];

    names.sort_by_key(|name| natural_sort_key(name));

    assert_eq!(
        names,
        vec!["img1.jpg", "img2.jpg", "img10.jpg", "IMG20.jpg"]
    );
}
