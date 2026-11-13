//! Embedded frontend assets and their cache policy.

use axum::http::{StatusCode, header};

use crate::common::{app, send, setup};

#[tokio::test]
async fn static_frontend_files_are_served() {
    let (dir, state) = setup().await;
    let app = app(state);

    // Vite emits content-hashed asset names, so discover them from the built
    // entry point instead of hardcoding names that change on every build.
    let index = std::fs::read_to_string("public/index.html").expect("built frontend is embedded");
    let asset_path = |marker: &str| -> String {
        let start = index
            .find(marker)
            .unwrap_or_else(|| panic!("index.html has no {marker:?}"));
        let rest = &index[start..];
        let value = rest
            .find("=\"./")
            .unwrap_or_else(|| panic!("index.html has no URL after {marker:?}"));
        let rest = &rest[value + 4..];
        let end = rest.find('"').expect("unterminated asset reference");
        format!("/public/{}", &rest[..end])
    };
    let js_path = asset_path("<script type=\"module\"");
    let css_path = asset_path("rel=\"stylesheet\"");
    let manifest_path = asset_path("rel=\"manifest\"");

    for (path, needle, content_type) in [
        (js_path.as_str(), "mediaHash", "javascript"),
        (css_path.as_str(), "--accent", "css"),
        (manifest_path.as_str(), "\"Wepics", "json"),
        ("/public/icon/circle.svg", "<svg", "svg"),
        ("/public/icon/circle16.png", "", "image/png"),
    ] {
        let (status, headers, body) = send(&app, path).await;
        assert_eq!(status, StatusCode::OK, "path: {path}");
        let got_type = headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap_or_default()
            .to_ascii_lowercase();
        assert!(
            got_type.contains(content_type),
            "path: {path}, content-type: {got_type}"
        );
        // Assets carry no content hash, so a stale cached copy would keep
        // running old JavaScript after an update.
        assert_eq!(
            headers[header::CACHE_CONTROL].to_str().unwrap_or_default(),
            "no-cache",
            "path: {path}"
        );
        if !needle.is_empty() {
            assert!(
                String::from_utf8_lossy(&body).contains(needle),
                "path: {path}"
            );
        }
    }

    let (status, headers, _) = send(&app, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers[header::CACHE_CONTROL].to_str().unwrap_or_default(),
        "no-cache"
    );

    // API and file namespaces must keep returning JSON/plain 404, not the SPA.
    let (status, headers, _) = send(&app, "/api/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap_or_default()
            .contains("json")
    );

    drop(dir);
}
