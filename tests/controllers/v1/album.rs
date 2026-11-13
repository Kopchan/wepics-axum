//! `GET /api/v1/album/{hash}` — stats, previews, children and ancestors.

use axum::http::StatusCode;
use chrono::{Duration, Utc};

use crate::common::{
    NewMedia, app, at, insert_child, insert_media, insert_typed_media, parse_v1, root_album_id,
    send, set_indexed_at, setup, setup_raw, v1_names, wait_for_indexed,
};
use wepics_axum::models::{MediaType, ROOT_ALBUM_HASH};

#[tokio::test]
async fn root_album_is_created_at_startup() {
    let (dir, state) = setup().await;
    let media_dir = state.media_dir.clone();
    let pool = state.db.clone();
    let app = app(state);

    assert!(media_dir.is_dir(), "media dir must exist after startup");

    let (status, _, body) = send(&app, "/api/v1/album/root").await;
    assert_eq!(status, StatusCode::OK);

    let album = parse_v1(&body);
    assert_eq!(album["hash"], ROOT_ALBUM_HASH);
    assert_eq!(album["name"], "root");

    // Second request reuses the row instead of creating a duplicate.
    let (status, _, body) = send(&app, "/api/v1/album/root").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(parse_v1(&body)["hash"], ROOT_ALBUM_HASH);

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

    let (status, _, _) = send(&app, "/api/v1/album/root").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let albums: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM albums")
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(albums, 0, "requests must not create albums");
    assert!(!state.media_dir.exists());

    // Startup is the only place that bootstraps storage.
    state.initialize().await.unwrap();
    let (status, _, _) = send(&app, "/api/v1/album/root").await;
    assert_eq!(status, StatusCode::OK);

    drop(dir);
}

#[tokio::test]
async fn unknown_album_is_not_found() {
    let (dir, state) = setup().await;
    let app = app(state);

    let (status, _, _) = send(&app, "/api/v1/album/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    drop(dir);
}

#[tokio::test]
async fn album_returns_stats_previews_and_signed_links() {
    let (dir, state) = setup().await;
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
    let (status, _, body) = send(&app, "/api/v1/album/root").await;
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
    let (status, _, body) = send(&app, "/api/v1/album/root?mlimit=0").await;
    assert_eq!(status, StatusCode::OK);
    assert!(parse_v1(&body).get("images").is_none());

    // A reverse-prefixed sort array is honoured.
    let (status, _, body) = send(&app, "/api/v1/album/root?msort=-size&mlimit=2").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        v1_names(&parse_v1(&body), "images"),
        vec!["photo6.jpg", "photo5.jpg"]
    );

    // Type filtering reaches the preview query too.
    let (status, _, body) = send(&app, "/api/v1/album/root?mtype=vid").await;
    assert_eq!(status, StatusCode::OK);
    assert!(parse_v1(&body)["images"].as_array().unwrap().is_empty());

    let (status, _, _) = send(&app, "/api/v1/album/root?mtype=bogus").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    drop(dir);
}

#[tokio::test]
async fn vectors_count_as_images_and_can_be_filtered() {
    let (dir, state) = setup().await;
    let root_id = root_album_id(&state).await;

    insert_media(
        &state.db,
        root_id,
        NewMedia {
            name: "photo.jpg",
            hash: "img",
            date: Some(at(1)),
            size: 10,
        },
    )
    .await;
    insert_typed_media(
        &state.db,
        root_id,
        "logo.svg",
        "vec",
        MediaType::Vector,
        64,
        32,
        Some(at(2)),
        5,
    )
    .await;

    let app = app(state.clone());

    let (status, _, body) = send(&app, "/api/v1/album/root").await;
    assert_eq!(status, StatusCode::OK);
    let album = parse_v1(&body);
    assert_eq!(album["mediasCount"], 2);
    assert_eq!(album["imagesCount"], 2, "vectors count as images");

    let types: Vec<&str> = album["images"]
        .as_array()
        .unwrap()
        .iter()
        .map(|image| image["type"].as_str().unwrap())
        .collect();
    assert_eq!(types, vec!["imageVector", "image"]);

    let (status, _, body) = send(&app, "/api/v1/album/root?mtype=vec&mlimit=10").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v1_names(&parse_v1(&body), "images"), vec!["logo.svg"]);

    drop(dir);
}

#[tokio::test]
async fn album_layers_children_sorting_and_ancestors() {
    let (dir, state) = setup().await;
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
    let (status, _, body) = send(&app, "/api/v1/album/root").await;
    assert_eq!(status, StatusCode::OK);
    let album = parse_v1(&body);
    assert_eq!(v1_names(&album, "children"), vec!["Alpha", "Bravo"]);
    assert_eq!(album["children"][0]["images"].as_array().unwrap().len(), 3);
    assert!(
        album["children"][0].get("children").is_none(),
        "grandchildren need `layers=2`"
    );

    // `asort` reverses child order.
    let (status, _, body) = send(&app, "/api/v1/album/root?asort=-name").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        v1_names(&parse_v1(&body), "children"),
        vec!["Bravo", "Alpha"]
    );

    // `layers=0` returns only the requested album.
    let (status, _, body) = send(&app, "/api/v1/album/root?layers=0").await;
    assert_eq!(status, StatusCode::OK);
    assert!(parse_v1(&body).get("children").is_none());

    // `layers=2` adds grandchildren; `mlimitnest` limits their previews.
    let (status, _, body) = send(&app, "/api/v1/album/root?layers=2&mlimitnest=1&mlimit=2").await;
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
    let (status, _, body) = send(&app, &format!("/api/v1/album/{}", grand.hash)).await;
    assert_eq!(status, StatusCode::OK);
    let album = parse_v1(&body);
    assert_eq!(
        v1_names(&album, "ancestors"),
        vec!["root", "Alpha"],
        "ancestors must be ordered from root down"
    );

    drop(dir);
}

#[tokio::test]
async fn album_syncs_only_when_stale() {
    let (dir, state) = setup().await;
    let root_id = root_album_id(&state).await;
    let app = app(state.clone());

    // Fresh album: no sync notice.
    let (status, _, body) = send(&app, "/api/v1/album/root").await;
    assert_eq!(status, StatusCode::OK);
    assert!(parse_v1(&body).get("sync").is_none());

    // 30 minutes old is still fresh.
    set_indexed_at(&state, root_id, Some(Utc::now() - Duration::minutes(30))).await;
    let (_, _, body) = send(&app, "/api/v1/album/root").await;
    assert!(parse_v1(&body).get("sync").is_none());

    // Older than an hour starts exactly one background task.
    let stale = Utc::now() - Duration::minutes(61);
    set_indexed_at(&state, root_id, Some(stale)).await;
    let (_, _, body) = send(&app, "/api/v1/album/root").await;
    assert_eq!(parse_v1(&body)["sync"], "started");

    wait_for_indexed(&state, root_id, stale).await;

    // Once done the index is fresh again: no second task.
    let (_, _, body) = send(&app, "/api/v1/album/root").await;
    assert!(parse_v1(&body).get("sync").is_none());

    drop(dir);
}
