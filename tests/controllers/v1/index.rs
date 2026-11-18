//! `GET|POST /api/v1/album/{hash}/index` — manual indexing and SSE progress.

use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use chrono::{DateTime, Utc};
use tokio_stream::StreamExt;
use tower::ServiceExt;

use crate::common::{
    app, create_dir_link, insert_album, link, parse_v1, root_album_id, send, send_with,
    set_indexed_at, setup, wait_for_indexed, wait_for_media_count,
};
use wepics_axum::models::{Album, Media, MediaType};

#[tokio::test]
async fn manual_index_honours_force_and_bare_flags() {
    let (dir, state) = setup().await;
    let album = insert_album(&state.db, "Trip", "trip").await;
    let album_dir = state.media_dir.join("trip");
    std::fs::create_dir_all(&album_dir).unwrap();
    std::fs::write(album_dir.join("a.jpg"), b"a").unwrap();
    set_indexed_at(&state, album.id, Some(Utc::now())).await;

    let app = app(state.clone());

    // Fresh album plus no `force`: nothing starts.
    let (status, _, body) = send(&app, "/api/v1/album/trip/index").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(parse_v1(&body)["sync"], "fresh");

    // Bare `?force&nested` mean true, also when sent with POST.
    let since = Utc::now() - chrono::Duration::seconds(1);
    let (status, _, body) = send_with(
        &app,
        Method::POST,
        "/api/v1/album/trip/index?force&nested",
        Default::default(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(parse_v1(&body)["sync"], "started");

    wait_for_indexed(&state, album.id, since).await;
    wait_for_media_count(&state, "trip", 1).await;

    // An explicit false disables the flag again.
    let (status, _, body) = send(&app, "/api/v1/album/trip/index?force=0").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(parse_v1(&body)["sync"], "fresh");

    let (status, _, _) = send(&app, "/api/v1/album/missing/index?force").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

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
    let (status, _, body) = send(&app, "/api/v1/album/trip/index?force&nested").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(parse_v1(&body)["sync"], "started");

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
                .uri("/api/v1/album/trip/index/events")
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

    let (status, _, _) = send(&app, "/api/v1/album/trip/index?force").await;
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
    assert!(text.contains("\"newMedia\":1"), "SSE output: {text}");
    assert!(text.contains("\"filesTotal\":1"), "SSE output: {text}");
    assert!(
        text.contains("\"albumFilesTotal\":1"),
        "SSE events carry per-album counters: {text}"
    );
    assert!(
        text.contains("\"current\":\"a.jpg\""),
        "SSE events name the file being hashed: {text}"
    );

    wait_for_indexed(&state, album.id, Utc::now() - chrono::Duration::seconds(1)).await;

    drop(dir);
}

#[tokio::test]
async fn global_index_sse_covers_every_album() {
    let (dir, state) = setup().await;
    let album = insert_album(&state.db, "Trip", "trip").await;
    let album_dir = state.media_dir.join("trip");
    std::fs::create_dir_all(&album_dir).unwrap();
    std::fs::write(album_dir.join("b.jpg"), b"b").unwrap();

    let app = app(state.clone());

    // A single subscription, not tied to an album hash.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/index/events")
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

    // The alias resolves to the same job; the stream is global.
    let (status, _, _) = send(&app, "/api/v1/album/trip/index?force").await;
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

    assert!(text.contains(&album.hash), "SSE output: {text}");
    assert!(text.contains("\"done\":true"), "SSE output: {text}");

    wait_for_indexed(&state, album.id, Utc::now() - chrono::Duration::seconds(1)).await;

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
    std::fs::write(media_dir.join("logo.svg"), b"<svg/>").unwrap();
    std::fs::write(media_dir.join("notes.txt"), b"ignored").unwrap();
    std::fs::write(media_dir.join(".hidden.jpg"), b"hidden").unwrap();

    std::fs::create_dir(media_dir.join("Winter")).unwrap();
    std::fs::write(media_dir.join("Winter").join("snow.jpg"), b"snow").unwrap();

    set_indexed_at(&state, root_id, None).await;
    let app = app(state.clone());
    let since = Utc::now() - chrono::Duration::seconds(1);

    let (_, _, body) = send(&app, "/api/v1/album/root").await;
    assert_eq!(parse_v1(&body)["sync"], "started");
    wait_for_indexed(&state, root_id, since).await;

    let rows: Vec<Media> = sqlx::query_as(
        "SELECT id, name, hash, \"type\", album_id, width, height, duration_sec, date, size,
                rating_level, sort_name, created_at, updated_at
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
            "logo.svg",
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
    assert_eq!(photo.hash.len(), 11, "media hash is XXH3-64 in Base64");
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
            .find(|media| media.name == "logo.svg")
            .unwrap()
            .r#type,
        MediaType::Vector
    );

    // The subdirectory became a child album with its own (not yet indexed) row.
    let winter: Album = sqlx::query_as(
        "SELECT id, name, hash, parent_id, path, alias, old_aliases, rating_level, order_level, guest_allow, indexed_at, sort_name, created_at, updated_at
         FROM albums WHERE path = 'Winter'",
    )
    .fetch_one(&state.db)
    .await
    .unwrap();
    assert_eq!(winter.name, "Winter");
    assert_eq!(winter.parent_id, Some(root_id));
    assert_eq!(winter.path, "Winter");
    assert_eq!(winter.indexed_at, None);
    assert_eq!(winter.hash.len(), 22, "album hash is a Base62 UUID v4");

    let (_, _, body) = send(&app, "/api/v1/album/root?alimit=10").await;
    let album = parse_v1(&body);
    assert_eq!(
        album["children"]
            .as_array()
            .unwrap()
            .iter()
            .map(|child| child["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["Winter"]
    );
    let winter_preview = &album["children"][0];
    assert!(
        winter_preview.get("indexedAt").is_none(),
        "an unindexed album must not serialize `indexedAt: null`"
    );
    assert!(
        winter_preview.get("stats").is_none(),
        "an album with no counters omits `stats` completely"
    );

    // Requesting the child indexes its own folder on demand.
    let winter_since = Utc::now() - chrono::Duration::seconds(1);
    let (_, _, body) = send(&app, &format!("/api/v1/album/{}", winter.hash)).await;
    assert_eq!(parse_v1(&body)["sync"], "started");
    wait_for_indexed(&state, winter.id, winter_since).await;

    let child_media: Vec<Media> = sqlx::query_as(
        "SELECT id, name, hash, \"type\", album_id, width, height, duration_sec, date, size,
                rating_level, sort_name, created_at, updated_at
         FROM media WHERE album_id = ?1",
    )
    .bind(winter.id)
    .fetch_all(&state.db)
    .await
    .unwrap();
    assert_eq!(child_media.len(), 1);
    assert_eq!(child_media[0].name, "snow.jpg");
    assert_eq!(child_media[0].hash.len(), 11);

    let query = link(&state, &winter.hash, &child_media[0].hash);
    let (status, _, body) = send(
        &app,
        &format!("/file/v1/{}/{}?{query}", winter.hash, child_media[0].hash),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"snow");

    let query = link(&state, "root", &photo.hash);
    let (status, _, body) = send(&app, &format!("/file/v1/root/{}?{query}", photo.hash)).await;
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
    let _ = send(&app, "/api/v1/album/root").await;
    wait_for_indexed(&state, root_id, since).await;

    std::fs::remove_file(media_dir.join("drop.jpg")).unwrap();
    std::fs::write(media_dir.join("new.png"), b"new").unwrap();

    let stale = Utc::now() - chrono::Duration::hours(2);
    set_indexed_at(&state, root_id, Some(stale)).await;
    let (_, _, body) = send(&app, "/api/v1/album/root").await;
    assert_eq!(parse_v1(&body)["sync"], "started");
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
async fn duplicates_share_one_media_row_and_are_counted() {
    let (dir, state) = setup().await;
    let root_id = root_album_id(&state).await;
    let media_dir = state.media_dir.clone();

    std::fs::write(media_dir.join("original.jpg"), b"same-content").unwrap();
    std::fs::write(media_dir.join("copy.jpg"), b"same-content").unwrap();

    set_indexed_at(&state, root_id, None).await;
    let app = app(state.clone());
    let since = Utc::now() - chrono::Duration::seconds(1);
    let _ = send(&app, "/api/v1/album/root").await;
    wait_for_indexed(&state, root_id, since).await;

    // Identical contents keep a single `media` row; the other copy points at it.
    let media: Vec<(i64, String, String)> =
        sqlx::query_as("SELECT id, name, hash FROM media WHERE album_id = ?1")
            .bind(root_id)
            .fetch_all(&state.db)
            .await
            .unwrap();
    assert_eq!(media.len(), 1, "only the owner stays in media");
    let (owner_id, owner_name, owner_hash) = &media[0];

    let duplicates: Vec<(String, i64, String)> =
        sqlx::query_as("SELECT name, media_id, data FROM duplica WHERE album_id = ?1")
            .bind(root_id)
            .fetch_all(&state.db)
            .await
            .unwrap();
    assert_eq!(duplicates.len(), 1, "the second copy becomes a duplica");
    assert_ne!(&duplicates[0].0, owner_name);
    assert_eq!(duplicates[0].1, *owner_id);
    let data: serde_json::Value = serde_json::from_str(&duplicates[0].2).unwrap();
    assert_eq!(data["hash"], owner_hash.as_str());

    let (_, _, body) = send(&app, "/api/v1/album/root?mlimit=10").await;
    let album = parse_v1(&body);
    assert_eq!(album["stats"]["direct"]["mediasCount"], 1);
    assert_eq!(album["stats"]["direct"]["duplicaCount"], 1);

    // Removing the copy drops the row and the counter.
    std::fs::remove_file(media_dir.join(&duplicates[0].0)).unwrap();
    let since = Utc::now();
    let (status, _, _) = send(&app, "/api/v1/album/root/index?force").await;
    assert_eq!(status, StatusCode::OK);
    wait_for_indexed(&state, root_id, since).await;

    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM duplica WHERE album_id = ?1")
        .bind(root_id)
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(left, 0);
    let (_, _, body) = send(&app, "/api/v1/album/root?mlimit=10").await;
    let album = parse_v1(&body);
    assert!(
        album["stats"]["direct"].get("duplicaCount").is_none(),
        "a zero duplicate counter is omitted"
    );

    drop(dir);
}

#[tokio::test]
async fn unchanged_files_keep_their_hash_without_reading_content() {
    let (dir, state) = setup().await;
    let root_id = root_album_id(&state).await;
    let path = state.media_dir.join("photo.jpg");
    std::fs::write(&path, b"first").unwrap();
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();

    set_indexed_at(&state, root_id, None).await;
    let app = app(state.clone());
    let since = Utc::now() - chrono::Duration::seconds(1);
    let _ = send(&app, "/api/v1/album/root").await;
    wait_for_indexed(&state, root_id, since).await;

    let before: String = sqlx::query_scalar("SELECT hash FROM media WHERE album_id = ?1")
        .bind(root_id)
        .fetch_one(&state.db)
        .await
        .unwrap();

    // Same size, different bytes, original mtime: the file looks untouched, so
    // its stored hash must be reused instead of hashing the new contents.
    std::fs::write(&path, b"other").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(modified)
        .unwrap();

    let since = Utc::now();
    let (status, _, _) = send(&app, "/api/v1/album/root/index?force").await;
    assert_eq!(status, StatusCode::OK);
    wait_for_indexed(&state, root_id, since).await;

    let after: String = sqlx::query_scalar("SELECT hash FROM media WHERE album_id = ?1")
        .bind(root_id)
        .fetch_one(&state.db)
        .await
        .unwrap();
    assert_eq!(
        before, after,
        "name + size + mtime are unchanged, so hashing must be skipped"
    );
    assert_ne!(
        wepics_axum::helpers::hashes::media_hash(&path).unwrap(),
        after,
        "the test overwrote the contents, so a real read would change the hash"
    );

    drop(dir);
}

#[tokio::test]
async fn symlinked_album_directory_is_indexed() {
    let (dir, state) = setup().await;
    let target = dir.path().join("linked-album");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("photo.jpg"), b"x").unwrap();

    let link_path = state.media_dir.join("Moe");
    if !create_dir_link(&link_path, &target) {
        eprintln!("skipping symlink test: cannot create directory link");
        return;
    }

    let app = app(state.clone());
    let root_id = root_album_id(&state).await;
    set_indexed_at(&state, root_id, None).await;
    let since = Utc::now() - chrono::Duration::seconds(1);
    let (status, _, _) = send(&app, "/api/v1/album/root/index?force").await;
    assert_eq!(status, StatusCode::OK);
    wait_for_indexed(&state, root_id, since).await;

    let moe: Album = sqlx::query_as(
        "SELECT id, name, hash, parent_id, path, alias, old_aliases, rating_level, order_level, guest_allow, indexed_at, sort_name, created_at, updated_at \
         FROM albums WHERE path = 'Moe'",
    )
    .fetch_one(&state.db)
    .await
    .unwrap();

    let since = Utc::now() - chrono::Duration::seconds(1);
    let (status, _, _) = send(&app, &format!("/api/v1/album/{}/index?force", moe.hash)).await;
    assert_eq!(status, StatusCode::OK);
    wait_for_indexed(&state, moe.id, since).await;
    wait_for_media_count(&state, "Moe", 1).await;

    let media_hash: String = sqlx::query_scalar("SELECT hash FROM media WHERE album_id = ?1")
        .bind(moe.id)
        .fetch_one(&state.db)
        .await
        .unwrap();
    let query = link(&state, &moe.hash, &media_hash);
    let (status, _, body) =
        send(&app, &format!("/file/v1/{}/{media_hash}?{query}", moe.hash)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"x");

    #[cfg(windows)]
    std::fs::remove_dir(&link_path).ok();
    #[cfg(unix)]
    std::fs::remove_file(&link_path).ok();

    drop(dir);
}

#[tokio::test]
async fn indexing_stores_image_and_vector_dimensions() {
    let (dir, state) = setup().await;
    let root_id = root_album_id(&state).await;

    crate::common::write_png(&state.media_dir.join("tiny.png"), 40, 20);
    std::fs::write(
        state.media_dir.join("logo.svg"),
        br#"<svg xmlns="http://www.w3.org/2000/svg" width="120" height="80"></svg>"#,
    )
    .unwrap();

    set_indexed_at(&state, root_id, None).await;
    let app = app(state.clone());
    let since = Utc::now() - chrono::Duration::seconds(1);
    let (status, _, _) = send(&app, "/api/v1/album/root/index?force").await;
    assert_eq!(status, StatusCode::OK);
    wait_for_indexed(&state, root_id, since).await;

    let rows: Vec<(String, String, i64, i64)> = sqlx::query_as(
        "SELECT name, \"type\", width, height FROM media WHERE album_id = ?1 ORDER BY name",
    )
    .bind(root_id)
    .fetch_all(&state.db)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            ("logo.svg".to_string(), "vector".to_string(), 120, 80),
            ("tiny.png".to_string(), "image".to_string(), 40, 20),
        ]
    );

    let (status, _, body) = send(&app, "/api/v1/album/root?mlimit=10").await;
    assert_eq!(status, StatusCode::OK);
    let album = parse_v1(&body);
    assert_eq!(album["stats"]["direct"]["imagesCount"], 2);
    assert_eq!(album["media"][0]["type"], "vector");
    assert_eq!(album["media"][0]["width"], 120);

    drop(dir);
}
