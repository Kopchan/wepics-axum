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
    let (status, _, body) = send(&app, "/api/v1/album/root?mlimit=4").await;
    assert_eq!(status, StatusCode::OK);

    let album = parse_v1(&body);
    assert_eq!(album["hash"], "root");
    assert_eq!(album["stats"]["direct"]["mediasCount"], 6);
    assert_eq!(album["stats"]["direct"]["imagesCount"], 6);
    assert!(
        album["stats"]["direct"].get("videosCount").is_none(),
        "zero counters are omitted"
    );
    assert!(
        album["stats"]["direct"].get("albumsCount").is_none(),
        "zero counters are omitted"
    );
    assert!(
        album["stats"].get("nested").is_none(),
        "an all-zero nested block is omitted; it covers sub-albums only"
    );
    assert!(
        album.get("orderLevel").is_none(),
        "orderLevel = 0 is omitted"
    );
    assert!(
        album.get("sync").is_none(),
        "fresh albums have no sync state"
    );

    // Four previews by default, naturally sorted by name.
    let images = album["media"].as_array().unwrap();
    assert_eq!(images.len(), 4);
    assert_eq!(
        v1_names(&album, "media"),
        vec!["photo1.jpg", "photo2.jpg", "photo3.jpg", "photo4.jpg"]
    );

    for image in images {
        assert_eq!(image["type"], "image");
        assert_eq!(image["width"], 800);
        assert_eq!(image["height"], 600);
        assert_eq!(image["ext"], "jpg");
        assert!(
            image.get("album").is_none(),
            "media of the requested album omit `album`"
        );
        assert!(
            (image["ratio"].as_f64().unwrap() - 800.0 / 600.0).abs() < 1e-9,
            "ratio is width / height"
        );

        let media_hash = image["hash"].as_str().unwrap();
        let token = image["sign"].as_str().unwrap();
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
    assert!(parse_v1(&body).get("media").is_none());

    // A reverse-prefixed sort array is honoured.
    let (status, _, body) = send(&app, "/api/v1/album/root?msort=-size&mlimit=2").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        v1_names(&parse_v1(&body), "media"),
        vec!["photo6.jpg", "photo5.jpg"]
    );

    // Type filtering reaches the preview query too.
    let (status, _, body) = send(&app, "/api/v1/album/root?mtype=vid&mlimit=10").await;
    assert_eq!(status, StatusCode::OK);
    assert!(parse_v1(&body)["media"].as_array().unwrap().is_empty());

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

    let (status, _, body) = send(&app, "/api/v1/album/root?mlimit=10").await;
    assert_eq!(status, StatusCode::OK);
    let album = parse_v1(&body);
    assert_eq!(album["stats"]["direct"]["mediasCount"], 2);
    assert_eq!(
        album["stats"]["direct"]["imagesCount"], 2,
        "vectors count as images"
    );

    let types: Vec<&str> = album["media"]
        .as_array()
        .unwrap()
        .iter()
        .map(|image| image["type"].as_str().unwrap())
        .collect();
    assert_eq!(types, vec!["vector", "image"]);

    let (status, _, body) = send(&app, "/api/v1/album/root?mtype=vec&mlimit=10").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v1_names(&parse_v1(&body), "media"), vec!["logo.svg"]);

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

    // No children or media unless `alimit`/`mlimit` ask for them.
    let (status, _, body) = send(&app, "/api/v1/album/root").await;
    assert_eq!(status, StatusCode::OK);
    let album = parse_v1(&body);
    assert!(album.get("children").is_none());
    assert!(album.get("media").is_none());

    // One layer of children, four previews on each child.
    let (status, _, body) = send(&app, "/api/v1/album/root?alimit=10&mlimit=0/4").await;
    assert_eq!(status, StatusCode::OK);
    let album = parse_v1(&body);
    assert_eq!(v1_names(&album, "children"), vec!["Alpha", "Bravo"]);
    assert_eq!(album["children"][0]["media"].as_array().unwrap().len(), 3);
    assert_eq!(album["stats"]["nested"]["mediasCount"], 9);
    assert!(
        album["children"][0].get("children").is_none(),
        "grandchildren need a second `alimit` layer"
    );

    // `asort` reverses child order.
    let (status, _, body) = send(&app, "/api/v1/album/root?alimit=10&asort=-name").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        v1_names(&parse_v1(&body), "children"),
        vec!["Bravo", "Alpha"]
    );

    // Two `alimit` layers add grandchildren; `mlimit` is per layer too.
    let (status, _, body) = send(&app, "/api/v1/album/root?alimit=10/10&mlimit=2/1/1").await;
    assert_eq!(status, StatusCode::OK);
    let album = parse_v1(&body);
    assert_eq!(album["media"].as_array().unwrap().len(), 2);
    let alpha = &album["children"][0];
    assert_eq!(alpha["name"], "Alpha");
    assert_eq!(alpha["media"].as_array().unwrap().len(), 1);
    let grand_preview = &alpha["children"][0];
    assert_eq!(grand_preview["name"], "Grand");
    assert_eq!(grand_preview["media"].as_array().unwrap().len(), 1);

    // Ancestors are root-first when `ancests` is requested.
    let (status, _, body) = send(&app, &format!("/api/v1/album/{}?ancests", grand.hash)).await;
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
async fn album_content_sort_uses_the_first_media_value() {
    let (dir, state) = setup().await;
    let root_id = root_album_id(&state).await;

    let alpha = insert_child(&state.db, root_id, "Alpha", at(1)).await;
    let bravo = insert_child(&state.db, root_id, "Bravo", at(1)).await;

    // Alpha's first media by date is older and smaller than Bravo's.
    for (album_id, prefix, date, size) in [
        (alpha.id, "a-old", at(1), 10),
        (alpha.id, "a-new", at(5), 50),
        (bravo.id, "b-old", at(2), 20),
        (bravo.id, "b-new", at(9), 90),
    ] {
        insert_media(
            &state.db,
            album_id,
            NewMedia {
                name: &format!("{prefix}.jpg"),
                hash: prefix,
                date: Some(date),
                size,
            },
        )
        .await;
    }

    let app = app(state.clone());

    // `content:<field>` orders albums by that field of their first media and
    // reports the value in `contentSort`.
    let (status, _, body) = send(&app, "/api/v1/album/root?alimit=10&asort=-content:date").await;
    assert_eq!(status, StatusCode::OK);
    let album = parse_v1(&body);
    assert_eq!(v1_names(&album, "children"), vec!["Bravo", "Alpha"]);
    assert_eq!(
        album["children"][0]["contentSort"], "2026-01-09T12:00:00+00:00",
        "the newest media date is shown"
    );
    assert_eq!(
        album["children"][1]["contentSort"],
        "2026-01-05T12:00:00+00:00"
    );

    let (status, _, body) = send(&app, "/api/v1/album/root?alimit=10&asort=content:size").await;
    assert_eq!(status, StatusCode::OK);
    let album = parse_v1(&body);
    assert_eq!(
        v1_names(&album, "children"),
        vec!["Alpha", "Bravo"],
        "ascending content:size uses the smallest media of each album"
    );
    assert_eq!(album["children"][0]["contentSort"], "10");
    assert_eq!(album["children"][1]["contentSort"], "20");

    let (status, _, body) = send(&app, "/api/v1/album/root?alimit=10&asort=-content:size").await;
    assert_eq!(status, StatusCode::OK);
    let album = parse_v1(&body);
    assert_eq!(v1_names(&album, "children"), vec!["Bravo", "Alpha"]);
    assert_eq!(album["children"][0]["contentSort"], "90");
    assert_eq!(album["children"][1]["contentSort"], "50");

    // Plain `content` keeps the legacy name ordering and exposes no numeric
    // `contentSort` value for the client to format.
    let (status, _, body) = send(&app, "/api/v1/album/root?alimit=10&asort=-content").await;
    assert_eq!(status, StatusCode::OK);
    let album = parse_v1(&body);
    assert_eq!(v1_names(&album, "children"), vec!["Bravo", "Alpha"]);
    assert!(
        album["children"][0].get("contentSort").is_none(),
        "a plain content sort has no field to report"
    );

    // Unknown content fields are rejected instead of silently ignored.
    let (status, _, _) = send(&app, "/api/v1/album/root?alimit=10&asort=content:random").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    drop(dir);
}

#[tokio::test]
async fn album_and_media_metadata_match_the_client_contract() {
    let (dir, state) = setup().await;
    let root_id = root_album_id(&state).await;
    let album = insert_child(&state.db, root_id, "Aliased", at(1)).await;
    let media = insert_media(
        &state.db,
        album.id,
        NewMedia {
            name: "cover.jpg",
            hash: "cover-m",
            date: Some(at(1)),
            size: 10,
        },
    )
    .await;

    sqlx::query(
        "UPDATE albums SET alias = 'canonical', old_aliases = '[\"old-one\",\"old-two\"]', \
         rating_level = 18, order_level = 7, guest_allow = 1 WHERE id = ?1",
    )
    .bind(album.id)
    .execute(&state.db)
    .await
    .unwrap();
    sqlx::query("UPDATE media SET rating_level = 12 WHERE id = ?1")
        .bind(media.id)
        .execute(&state.db)
        .await
        .unwrap();

    let app = app(state);

    // The canonical alias addresses the same album as its hash.
    let (status, _, body) = send(&app, "/api/v1/album/canonical?mlimit=10").await;
    assert_eq!(status, StatusCode::OK);
    let album_body = parse_v1(&body);
    assert_eq!(album_body["hash"], album.hash);
    assert_eq!(album_body["alias"], "canonical");
    assert_eq!(album_body["ratingLevel"], 18);
    assert_eq!(album_body["orderLevel"], 7);
    assert_eq!(album_body["guestAllow"], true);
    assert_eq!(album_body["oldAliases"][0], "old-one");
    assert_eq!(album_body["oldAliases"][1], "old-two");

    let preview = &album_body["media"][0];
    assert_eq!(preview["ratingLevel"], 12);
    assert!(
        preview.get("album").is_none(),
        "media of the requested album omit `album`"
    );

    // Media from a child album names its own album, so the client can build a
    // scoped viewer link.
    let (status, _, body) = send(&app, "/api/v1/album/root?alimit=10&mlimit=0/10").await;
    assert_eq!(status, StatusCode::OK);
    let root_body = parse_v1(&body);
    let nested = &root_body["children"][0]["media"][0];
    assert_eq!(nested["hash"], media.hash);
    assert_eq!(nested["album"]["hash"], album.hash);
    assert_eq!(nested["album"]["alias"], "canonical");
    assert_eq!(nested["album"]["ratingLevel"], 18);

    // The single-media endpoint exposes the same metadata.
    let (status, _, body) = send(
        &app,
        &format!("/api/v1/album/{}/media/{}", album.hash, media.hash),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let media_body = parse_v1(&body);
    assert_eq!(media_body["ratingLevel"], 12);
    assert!(
        media_body.get("album").is_none(),
        "the path already identifies the album"
    );

    drop(dir);
}

#[tokio::test]
async fn childs_lists_direct_children_with_layer_options() {
    let (dir, state) = setup().await;
    let root_id = root_album_id(&state).await;

    let alpha = insert_child(&state.db, root_id, "Alpha", at(1)).await;
    let bravo = insert_child(&state.db, root_id, "Bravo", at(2)).await;
    let grand = insert_child(&state.db, alpha.id, "Grand", at(1)).await;

    for (album_id, prefix) in [
        (alpha.id, "alpha"),
        (bravo.id, "bravo"),
        (grand.id, "grand"),
    ] {
        for index in 1..=2 {
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

    let app = app(state);

    let (status, _, body) = send(&app, "/api/v1/album/root/childs?alimit=10&mlimit=2").await;
    assert_eq!(status, StatusCode::OK);
    let page = parse_v1(&body);
    assert_eq!(page["total"], 2);
    assert_eq!(v1_names(&page, "items"), vec!["Alpha", "Bravo"]);
    assert_eq!(page["items"][0]["media"].as_array().unwrap().len(), 2);
    assert_eq!(
        v1_names(&page["items"][0], "children"),
        vec!["Grand"],
        "`alimit` layer 0 applies to every returned child"
    );

    let (status, _, body) = send(&app, "/api/v1/album/root/childs?limit=1&page=2").await;
    assert_eq!(status, StatusCode::OK);
    let page = parse_v1(&body);
    assert_eq!(page["total"], 2);
    assert_eq!(v1_names(&page, "items"), vec!["Bravo"]);

    drop(dir);
}

#[tokio::test]
async fn mnest_includes_media_from_descendants() {
    let (dir, state) = setup().await;
    let root_id = root_album_id(&state).await;
    let alpha = insert_child(&state.db, root_id, "Alpha", at(1)).await;
    let grand = insert_child(&state.db, alpha.id, "Grand", at(1)).await;
    let deep = insert_child(&state.db, grand.id, "Deep", at(1)).await;

    insert_media(
        &state.db,
        alpha.id,
        NewMedia {
            name: "alpha.jpg",
            hash: "alpha-m",
            date: Some(at(1)),
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
            date: Some(at(2)),
            size: 10,
        },
    )
    .await;
    insert_media(
        &state.db,
        deep.id,
        NewMedia {
            name: "deep.jpg",
            hash: "deep-m",
            date: Some(at(3)),
            size: 10,
        },
    )
    .await;

    let app = app(state);

    // Layer 1 is Alpha; without `mnest` only its own media appears.
    let (_, _, body) = send(&app, "/api/v1/album/root?alimit=10&mlimit=0/10").await;
    let album = parse_v1(&body);
    assert_eq!(v1_names(&album["children"][0], "media"), vec!["alpha.jpg"]);

    // `mnest=-/+` enables nesting for the child layer, for the whole subtree.
    let (_, _, body) = send(&app, "/api/v1/album/root?alimit=10&mlimit=0/10&mnest=-/+").await;
    let album = parse_v1(&body);
    let previews = &album["children"][0]["media"];
    assert_eq!(
        v1_names(&album["children"][0], "media"),
        vec!["alpha.jpg", "deep.jpg", "grand.jpg"],
        "`mnest` walks every descendant, not just direct children"
    );
    assert_eq!(
        previews[0]["album"]["hash"], alpha.hash,
        "only media of the requested album omit `album`"
    );
    assert_eq!(previews[1]["album"]["hash"], deep.hash);
    assert_eq!(previews[2]["album"]["hash"], grand.hash);

    drop(dir);
}

#[tokio::test]
async fn childs_sort_by_materialized_stats() {
    let (dir, state) = setup().await;
    let root_id = root_album_id(&state).await;

    let few = insert_child(&state.db, root_id, "Few", at(1)).await;
    let many = insert_child(&state.db, root_id, "Many", at(2)).await;

    insert_media(
        &state.db,
        few.id,
        NewMedia {
            name: "few.jpg",
            hash: "few-m",
            date: Some(at(1)),
            size: 10,
        },
    )
    .await;
    for index in 0..3 {
        insert_media(
            &state.db,
            many.id,
            NewMedia {
                name: &format!("many-{index}.jpg"),
                hash: &format!("many-m{index}"),
                date: Some(at(index + 1)),
                size: 100,
            },
        )
        .await;
    }

    let app = app(state);

    let (status, _, body) = send(&app, "/api/v1/album/root/childs?asort=-medias").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v1_names(&parse_v1(&body), "items"), vec!["Many", "Few"]);

    let (status, _, body) = send(&app, "/api/v1/album/root/childs?asort=size").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v1_names(&parse_v1(&body), "items"), vec!["Few", "Many"]);

    // Counters reflect trigger-maintained rows, not per-row media scans.
    let (status, _, body) = send(&app, "/api/v1/album/root/childs?asort=-medias").await;
    assert_eq!(status, StatusCode::OK);
    let page = parse_v1(&body);
    assert_eq!(page["items"][0]["stats"]["direct"]["mediasCount"], 3);
    assert_eq!(page["items"][0]["stats"]["direct"]["size"], 300);
    assert_eq!(page["items"][1]["stats"]["direct"]["mediasCount"], 1);

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
