//! Embedded frontend assets and their cache policy.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};

use wepics_axum::config::AppConfig;
use wepics_axum::layers::static_cache::precompress_public;
use wepics_axum::state::AppState;

use crate::common::{app, send, send_with, setup, test_pool, workspace_tempdir};

/// Resolves a URL from `public/index.html` to a request path.
///
/// The server build references assets from the root (`/assets/...`), while a
/// relative build uses `./assets/...`; both are valid.
fn asset_url(index: &str, marker: &str) -> String {
    let start = index
        .find(marker)
        .unwrap_or_else(|| panic!("index.html has no {marker:?}"));
    let rest = &index[start..];

    if let Some(value) = rest.find("=\"/") {
        let rest = &rest[value + 2..];
        let end = rest.find('"').expect("unterminated asset reference");
        return rest[..end].to_string();
    }

    let value = rest
        .find("=\"./")
        .unwrap_or_else(|| panic!("index.html has no URL after {marker:?}"));
    let rest = &rest[value + 4..];
    let end = rest.find('"').expect("unterminated asset reference");
    format!("/public/{}", &rest[..end])
}

/// Request path -> file under `public/`.
fn source_path(url: &str) -> PathBuf {
    Path::new("public").join(url.trim_start_matches("/public/").trim_start_matches('/'))
}

/// Cached `.br` file for a request path.
fn compressed_path(config: &AppConfig, url: &str, extension: &str) -> PathBuf {
    let relative = url.trim_start_matches("/public/").trim_start_matches('/');
    config
        .compressed_public_dir()
        .join(format!("{relative}.{extension}"))
}

fn accept_encoding(value: &'static str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(header::ACCEPT_ENCODING, HeaderValue::from_static(value));
    headers
}

fn brotli_text(data: &[u8]) -> String {
    let mut out = Vec::new();
    brotli::Decompressor::new(data, 4096)
        .read_to_end(&mut out)
        .unwrap();
    String::from_utf8(out).unwrap()
}

#[tokio::test]
async fn static_frontend_files_are_served() {
    let (dir, state) = setup().await;
    let app = app(state);

    // Vite emits content-hashed asset names, so discover them from the built
    // entry point instead of hardcoding names that change on every build.
    let index = std::fs::read_to_string("public/index.html").expect("built frontend is present");
    let js_path = asset_url(&index, "<script type=\"module\"");
    let css_path = asset_url(&index, "rel=\"stylesheet\"");
    let manifest_path = asset_url(&index, "rel=\"manifest\"");

    for (path, needle, content_type) in [
        (js_path.as_str(), "createApp", "javascript"),
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

        // Files under `assets/` carry a content hash and never change. Other
        // shell files are revalidated so an update is picked up.
        let expected = if path.contains("/assets/") {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        };
        assert_eq!(
            headers[header::CACHE_CONTROL].to_str().unwrap_or_default(),
            expected,
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

#[tokio::test]
async fn compressed_static_is_persisted_and_served_from_disk() {
    let (dir, state) = setup().await;
    let app = app(state.clone());

    let index = std::fs::read_to_string("public/index.html").expect("built frontend is present");
    let js_path = asset_url(&index, "<script type=\"module\"");
    let source = source_path(&js_path);
    let source_body = std::fs::read(&source).expect("asset exists");

    // Startup precompression, like `main` does before opening the ports.
    let written = precompress_public(state.config.clone()).await.unwrap();
    assert!(written > 0, "an empty cache must be populated");

    let br_file = compressed_path(&state.config, &js_path, "br");
    let gz_file = compressed_path(&state.config, &js_path, "gz");
    assert!(br_file.is_file(), "{} must exist", br_file.display());
    assert!(gz_file.is_file(), "{} must exist", gz_file.display());

    // A second startup pass must reuse the files instead of rewriting them.
    let rewritten = precompress_public(state.config.clone()).await.unwrap();
    assert_eq!(rewritten, 0, "fresh compressed files must not be rewritten");

    // Brotli request is served from the cached file.
    let (status, headers, body) =
        send_with(&app, Method::GET, &js_path, accept_encoding("gzip, br")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers[header::CONTENT_ENCODING].to_str().unwrap(),
        "br",
        "brotli is preferred"
    );
    assert_eq!(headers[header::VARY].to_str().unwrap(), "Accept-Encoding");
    assert_eq!(
        headers[header::CONTENT_LENGTH].to_str().unwrap(),
        std::fs::metadata(&br_file).unwrap().len().to_string()
    );

    let mut decompressed = Vec::new();
    brotli::Decompressor::new(&body[..], 4096)
        .read_to_end(&mut decompressed)
        .unwrap();
    assert_eq!(decompressed, source_body);

    // Conditional revalidation uses the cached ETag.
    let etag = headers[header::ETAG].clone();
    let mut conditional = accept_encoding("br");
    conditional.insert(header::IF_NONE_MATCH, etag);
    let (status, headers, body) = send_with(&app, Method::GET, &js_path, conditional).await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);
    assert!(body.is_empty());
    assert_eq!(
        headers[header::CACHE_CONTROL].to_str().unwrap(),
        "public, max-age=31536000, immutable"
    );

    // Gzip gets its own persisted representation.
    let (status, headers, body) =
        send_with(&app, Method::GET, &js_path, accept_encoding("gzip")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_ENCODING].to_str().unwrap(), "gzip");
    let mut decompressed = Vec::new();
    flate2::read::GzDecoder::new(&body[..])
        .read_to_end(&mut decompressed)
        .unwrap();
    assert_eq!(decompressed, source_body);

    drop(dir);
}

#[tokio::test]
async fn changed_static_file_is_recompressed_on_the_fly() {
    let dir = workspace_tempdir();
    let public = dir.path().join("public");
    std::fs::create_dir_all(public.join("assets")).unwrap();

    let file = public.join("assets").join("app.js");
    std::fs::write(&file, "console.log('one');\n".repeat(64)).unwrap();

    let mut config = AppConfig::defaults();
    config.media_dir = dir.path().join("media");
    config.cache_dir = dir.path().join("cache");
    config.public_dir = public.clone();
    let config = Arc::new(config);

    let pool = test_pool().await;
    let state = AppState::new(pool, config.clone());
    let app = app(state);

    precompress_public(config.clone()).await.unwrap();
    let cached = compressed_path(&config, "/assets/app.js", "br");
    assert!(cached.is_file());

    let (status, _, body) =
        send_with(&app, Method::GET, "/assets/app.js", accept_encoding("br")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(brotli_text(&body).contains("one"));

    // Rewrite the source, as a redeploy would.
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(&file, "console.log('two');\n".repeat(64)).unwrap();

    let (status, _, body) =
        send_with(&app, Method::GET, "/assets/app.js", accept_encoding("br")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        brotli_text(&body).contains("two"),
        "changed source must be recompressed"
    );

    // The persisted copy was updated too, not just the response.
    let bytes = std::fs::read(&cached).unwrap();
    assert!(brotli_text(&bytes).contains("two"));

    drop(dir);
}
