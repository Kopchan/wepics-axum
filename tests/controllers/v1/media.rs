//! `GET /api/v1/album/{hash}/media` — pagination, sorting and type filters.

use axum::http::StatusCode;

use crate::common::{
    NewMedia, app, at, insert_child, insert_media, insert_typed_media, parse_v1, root_album_id,
    send, setup, v1_names,
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
    assert_eq!(page["total"], 5);
    assert!(
        page.get("page").is_none(),
        "lists return `{{total, items}}`"
    );
    assert_eq!(v1_names(&page, "items"), vec!["photo3.jpg", "photo4.jpg"]);

    let (status, _, body) = send(&app, "/api/v1/album/root/media?sort=-size").await;
    assert_eq!(status, StatusCode::OK);
    let page = parse_v1(&body);
    assert_eq!(
        v1_names(&page, "items"),
        vec![
            "clip.mp4",
            "photo5.jpg",
            "photo4.jpg",
            "photo3.jpg",
            "photo2.jpg",
            "photo1.jpg"
        ]
    );

    let picture = &page["items"][0];
    let media_hash = picture["hash"].as_str().unwrap();
    let token = picture["sign"].as_str().unwrap();
    let exp = picture["exp"].as_i64().unwrap();
    assert!(state.verify_media("root", media_hash, token, exp));

    let (status, _, body) = send(&app, "/api/v1/album/root/media?limit=*&sort=name").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v1_names(&parse_v1(&body), "items").len(), 6);

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
    let names = v1_names(&parse_v1(&body), "items");
    assert_eq!(names, vec!["img1.jpg", "img2.jpg", "img10.jpg"]);

    let (_, _, body) = send(&app, "/api/v1/album/abc123/media?sort=-name").await;
    let names = v1_names(&parse_v1(&body), "items");
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
    assert_eq!(
        v1_names(&parse_v1(&body), "items"),
        vec!["c.jpg", "b.jpg", "a.jpg"]
    );

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
    assert_eq!(v1_names(&parse_v1(&body), "items"), vec!["logo.svg"]);

    let (_, _, body) = send(&app, "/api/v1/album/root/media?type=img,vec").await;
    assert_eq!(v1_names(&parse_v1(&body), "items").len(), 2);

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

#[tokio::test]
async fn nest_includes_descendant_media() {
    let (dir, state) = setup().await;
    let root_id = root_album_id(&state).await;
    let child = insert_child(&state.db, root_id, "Child", at(1)).await;
    let grand = insert_child(&state.db, child.id, "Grand", at(1)).await;

    insert_media(
        &state.db,
        root_id,
        NewMedia {
            name: "root.jpg",
            hash: "root-m",
            date: Some(at(1)),
            size: 10,
        },
    )
    .await;
    insert_media(
        &state.db,
        child.id,
        NewMedia {
            name: "child.jpg",
            hash: "child-m",
            date: Some(at(2)),
            size: 10,
        },
    )
    .await;
    insert_media(
        &state.db,
        grand.id,
        NewMedia {
            name: "grand.jpg",
            hash: "grand-m",
            date: Some(at(3)),
            size: 10,
        },
    )
    .await;

    let app = app(state);

    let (_, _, body) = send(&app, "/api/v1/album/root/media?sort=name").await;
    assert_eq!(v1_names(&parse_v1(&body), "items"), vec!["root.jpg"]);

    let (status, _, body) = send(&app, "/api/v1/album/root/media?sort=name&nest").await;
    assert_eq!(status, StatusCode::OK);
    let page = parse_v1(&body);
    assert_eq!(page["total"], 3, "nest walks the whole subtree");
    assert_eq!(
        v1_names(&page, "items"),
        vec!["child.jpg", "grand.jpg", "root.jpg"]
    );
    assert_eq!(
        page["items"][0]["album"]["hash"], child.hash,
        "nested media name their own album"
    );
    assert_eq!(page["items"][1]["album"]["hash"], grand.hash);
    assert!(
        page["items"][2].get("album").is_none(),
        "media of the requested album omit `album`"
    );

    drop(dir);
}

#[tokio::test]
async fn single_media_info_carries_signature_without_album() {
    let (dir, state) = setup().await;
    let root_id = root_album_id(&state).await;

    insert_media(
        &state.db,
        root_id,
        NewMedia {
            name: "photo1.jpg",
            hash: "m1",
            date: Some(at(1)),
            size: 10,
        },
    )
    .await;

    let app = app(state.clone());
    let (status, _, body) = send(&app, "/api/v1/album/root/media/m1").await;
    assert_eq!(status, StatusCode::OK);
    let media = parse_v1(&body);
    assert_eq!(media["hash"], "m1");
    assert_eq!(media["name"], "photo1");
    assert_eq!(media["ext"], "jpg");
    assert_eq!(media["type"], "image");
    assert!(
        media.get("album").is_none(),
        "the request path already names the album"
    );

    let exp = media["exp"].as_i64().unwrap();
    assert!(state.verify_media("root", "m1", media["sign"].as_str().unwrap(), exp));

    let (status, _, _) = send(&app, "/api/v1/album/root/media/missing").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    drop(dir);
}

async fn insert_media_album(state: &wepics_axum::state::AppState) -> i64 {
    crate::common::insert_album(&state.db, "Trip", "abc123")
        .await
        .id
}
