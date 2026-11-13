//! SPA shell and bot-facing Open Graph pages.

use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};

use crate::common::{NewMedia, app, at, insert_album, insert_media, send, send_with, setup};

#[tokio::test]
async fn root_and_album_pages_are_spa_for_people_and_og_for_bots() {
    let (dir, state) = setup().await;
    let album = insert_album(&state.db, "Trip & Fun", "trip").await;
    insert_media(
        &state.db,
        album.id,
        NewMedia {
            name: "cat.jpg",
            hash: "h1",
            date: Some(at(1)),
            size: 8,
        },
    )
    .await;

    let app = app(state);

    let (status, headers, body) = send(&app, "/trip").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/html")
    );
    let html = String::from_utf8_lossy(&body);
    assert!(html.contains("id=\"app\""), "SPA shell expected");

    let (status, _, body) = send(&app, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&body).contains("id=\"app\""));

    let mut bot = HeaderMap::new();
    bot.insert(
        header::USER_AGENT,
        HeaderValue::from_static("Twitterbot/1.0"),
    );
    let (status, _, body) = send_with(&app, Method::GET, "/trip", bot.clone()).await;
    assert_eq!(status, StatusCode::OK);
    let html = String::from_utf8_lossy(&body);
    assert!(html.contains("og:title"), "OG title missing: {html}");
    assert!(
        html.contains("Trip &amp; Fun"),
        "album name must be escaped"
    );
    assert!(
        html.contains("/file/v1/trip/h1?hash="),
        "OG image must use the signed v1 file link: {html}"
    );
    assert!(html.contains("og:image:width"));

    let (status, _, body) = send_with(&app, Method::GET, "/", bot).await;
    assert_eq!(status, StatusCode::OK);
    let html = String::from_utf8_lossy(&body);
    assert!(html.contains("og:site_name"));
    assert!(html.contains("og:title"));

    let mut bot = HeaderMap::new();
    bot.insert(
        header::USER_AGENT,
        HeaderValue::from_static("Googlebot/2.1"),
    );
    let (status, _, body) = send_with(&app, Method::GET, "/missing", bot).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(String::from_utf8_lossy(&body).contains("404"));

    // API and media namespaces must not fall through to the SPA.
    for path in ["/api/nope", "/file/nope", "/thumb/nope"] {
        let (status, headers, _) = send(&app, path).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "path: {path}");
        assert!(
            !headers[header::CONTENT_TYPE]
                .to_str()
                .unwrap_or_default()
                .contains("html"),
            "path: {path} must not return the shell"
        );
    }

    drop(dir);
}
