pub mod api;
pub mod files;

use axum::Router;
use axum::routing::get;
use tower_http::services::ServeDir;

use crate::controllers;
use crate::cors;
use crate::state::AppState;

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
        .nest("/api", api::router())
        .nest("/files", files::router())
        .fallback_service(static_files)
        .layer(cors::layer())
        .with_state(state)
}
