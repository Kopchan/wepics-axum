//! CORS headers on the API and media routes.

use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};

use crate::common::{app, send, send_with, setup};

#[tokio::test]
async fn cors_is_enabled_for_every_route() {
    let (dir, state) = setup().await;
    let app = app(state);

    let (status, headers, _) = send(&app, "/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
    let exposed = headers[header::ACCESS_CONTROL_EXPOSE_HEADERS]
        .to_str()
        .unwrap();
    assert!(exposed.contains("content-range"), "exposed: {exposed}");
    assert!(exposed.contains("etag"), "exposed: {exposed}");
    assert!(exposed.contains("cache-control"), "exposed: {exposed}");

    let mut preflight = HeaderMap::new();
    preflight.insert(
        header::ORIGIN,
        HeaderValue::from_static("https://example.com"),
    );
    preflight.insert(
        header::ACCESS_CONTROL_REQUEST_METHOD,
        HeaderValue::from_static("GET"),
    );
    preflight.insert(
        header::ACCESS_CONTROL_REQUEST_HEADERS,
        HeaderValue::from_static("range"),
    );

    let (status, headers, body) =
        send_with(&app, Method::OPTIONS, "/file/v1/abc123/h1", preflight).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
    let methods = headers[header::ACCESS_CONTROL_ALLOW_METHODS]
        .to_str()
        .unwrap();
    assert!(methods.contains("GET"), "methods: {methods}");
    assert!(
        headers[header::ACCESS_CONTROL_ALLOW_HEADERS]
            .to_str()
            .unwrap()
            .contains("range")
    );
    assert_eq!(headers[header::ACCESS_CONTROL_MAX_AGE], "86400");
    assert!(body.is_empty());

    drop(dir);
}
