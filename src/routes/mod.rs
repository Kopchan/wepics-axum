pub mod v1;

use axum::Router;
use axum::extract::Request;
use axum::http::HeaderValue;
use axum::http::header::CACHE_CONTROL;
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::get;
use tower_http::compression::CompressionLayer;
use tower_http::services::ServeDir;

use crate::controllers;
use crate::cors;
use crate::state::AppState;

/// Hashed build assets never change under the same URL, so they can be cached
/// forever; everything else in the shell is revalidated so an update is picked
/// up on the next load. `no-cache` still allows cheap `304 Not Modified`
/// revalidation via the `ETag`/`Last-Modified` headers.
async fn revalidate_static(req: Request, next: Next) -> Response {
    let path = req.uri().path().to_owned();
    let mut response = next.run(req).await;

    // Media (`/file`, `/files`, `/thumb`) is immutable content and API
    // responses are dynamic; only the shell and its assets need caching rules.
    if !(path.starts_with("/api") || path.starts_with("/file") || path.starts_with("/thumb")) {
        let cache_control = if path.starts_with("/assets/") || path.starts_with("/public/assets/")
        {
            // Vite puts a content hash in every file name under `assets/`.
            HeaderValue::from_static("public, max-age=31536000, immutable")
        } else {
            HeaderValue::from_static("no-cache")
        };
        response.headers_mut().insert(CACHE_CONTROL, cache_control);
    }

    response
}

/// Full application: static frontend, SPA/SSR pages, API, files and CORS.
pub fn app(state: AppState) -> Router {
    let public_dir = controllers::public_dir();
    let spa = get(controllers::spa).with_state(state.clone());

    // `fallback` keeps the status of the SPA response (200), unlike
    // `not_found_service`, which would force every page to 404.
    let static_files = ServeDir::new(&public_dir)
        .append_index_html_on_directories(false)
        .fallback(spa);

    Router::new()
        .route("/", get(controllers::root_page))
        .route("/health", get(controllers::health))
        // Assets are referenced from the root (`/app.js`), but `/public/...`
        // is also served so the frontend can be addressed explicitly.
        .nest_service(
            "/public",
            ServeDir::new(&public_dir).append_index_html_on_directories(false),
        )
        .nest("/api/v1", v1::api::router())
        .nest("/file/v1", v1::files::router())
        .nest("/thumb/v1", v1::thumb::router())
        .fallback_service(static_files)
        .layer(cors::layer())
        .layer(middleware::from_fn(revalidate_static))
        // Text responses (JS/CSS/JSON/HTML) are compressed; images, media
        // ranges and SSE are skipped by the default predicate.
        .layer(CompressionLayer::new())
        .with_state(state)
}
