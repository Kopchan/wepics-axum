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
    let static_files = ServeDir::new(controllers::public_dir())
        .append_index_html_on_directories(false)
        .not_found_service(get(controllers::spa).with_state(state.clone()));

    Router::new()
        .route("/", get(controllers::root_page))
        .route("/health", get(controllers::health))
        // .nest_service("/public", ServeDir::new(controllers::public_dir()))
        .nest("/api", api::router())
        .nest("/files", files::router())
        // .fallback(get(controllers::spa))
        .fallback_service(static_files)
        .layer(cors::layer())
        .with_state(state)
}
