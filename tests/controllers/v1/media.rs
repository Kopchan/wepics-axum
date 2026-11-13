//! `GET /api/v1/album/{hash}/media` — pagination, sorting and type filters.

use axum::http::StatusCode;

use crate::common::{
    NewMedia, app, at, insert_media, insert_typed_media, parse_v1, root_album_id, send, setup,
    v1_names,
};
use wepics_axum::models::MediaType;

#[tokio::test]
async fn media_paginates_sorts_and_filters() {
    let (dir, state) = setup().await;
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
    insert_typed_media(
        &state.db,
        root_id,
        "clip.mp4",
        "v1",
        MediaType::Video,
        0,
        0,
        None,
        100,
    )
    .await;

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

    drop(dir);
}

#[tokio::test]
async fn name_sort_is_natural_and_reversible() {
    let (dir, state) = setup().await;
    let album = insert_media_album(&state).await;

    for (index, name) in ["img10.jpg", "img2.jpg", "img1.jpg"].iter().enumerate() {
        insert_media(
            &state.db,
            album,
            NewMedia {
                name,
                hash: name,
                date: Some(at(index as u32 + 1)),
                size: 10,
            },
        )
        .await;
    }

    let app = app(state);

    let (_, _, body) = send(&app, "/api/v1/album/abc123/media?sort=name").await;
    let names: Vec<_> = parse_v1(&body)["pictures"]
        .as_array()
        .unwrap()
        .iter()
        .map(|media| media["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, vec!["img1.jpg", "img2.jpg", "img10.jpg"]);

    let (_, _, body) = send(&app, "/api/v1/album/abc123/media?sort=-name").await;
    let names: Vec<_> = parse_v1(&body)["pictures"]
        .as_array()
        .unwrap()
        .iter()
        .map(|media| media["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, vec!["img10.jpg", "img2.jpg", "img1.jpg"]);

    drop(dir);
}

#[tokio::test]
async fn size_sort_is_supported() {
    let (dir, state) = setup().await;
    let album = insert_media_album(&state).await;

    for (name, size) in [("b.jpg", 200), ("a.jpg", 100), ("c.jpg", 300)] {
        insert_media(
            &state.db,
            album,
            NewMedia {
                name,
                hash: name,
                date: Some(at(1)),
                size,
            },
        )
        .await;
    }

    let app = app(state);
    let (_, _, body) = send(&app, "/api/v1/album/abc123/media?sort=-size").await;
    let names: Vec<_> = parse_v1(&body)["pictures"]
        .as_array()
        .unwrap()
        .iter()
        .map(|media| media["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, vec!["c.jpg", "b.jpg", "a.jpg"]);

    drop(dir);
}

#[tokio::test]
async fn type_filter_accepts_vectors() {
    let (dir, state) = setup().await;
    let root_id = root_album_id(&state).await;

    insert_typed_media(
        &state.db,
        root_id,
        "logo.svg",
        "vec",
        MediaType::Vector,
        10,
        10,
        Some(at(1)),
        5,
    )
    .await;
    insert_media(
        &state.db,
        root_id,
        NewMedia {
            name: "photo.jpg",
            hash: "img",
            date: Some(at(2)),
            size: 10,
        },
    )
    .await;

    let app = app(state);

    let (status, _, body) = send(&app, "/api/v1/album/root/media?type=vec").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v1_names(&parse_v1(&body), "pictures"), vec!["logo.svg"]);

    let (_, _, body) = send(&app, "/api/v1/album/root/media?type=img,vec").await;
    assert_eq!(v1_names(&parse_v1(&body), "pictures").len(), 2);

    drop(dir);
}

#[tokio::test]
async fn invalid_pagination_and_sort_are_bad_requests() {
    let (dir, state) = setup().await;
    insert_media_album(&state).await;
    let app = app(state);

    for uri in [
        "/api/v1/album/abc123/media?page=0",
        "/api/v1/album/abc123/media?limit=0",
        "/api/v1/album/abc123/media?limit=101",
        "/api/v1/album/abc123/media?sort=bogus",
        "/api/v1/album/abc123/media?sort=--name",
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

async fn insert_media_album(state: &wepics_axum::state::AppState) -> i64 {
    crate::common::insert_album(&state.db, "Trip", "abc123")
        .await
        .id
}
