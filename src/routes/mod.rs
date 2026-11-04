pub mod api;
pub mod files;

use axum::Router;
use axum::routing::get;
use tower_http::services::ServeDir;

use crate::controllers;
use crate::cors;
use crate::state::AppState;

/// Everything except the trivial root/health routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .nest("/api", api::router())
        .nest("/files", files::router())
}

/// Full application: static frontend, SPA/SSR pages, API, files and CORS.
pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/", get(controllers::root_page))
        .route("/favicon.ico", get(controllers::favicon))
        .route("/hello/{name}", get(controllers::hello_name))
        .route("/health", get(controllers::health))
        .nest_service("/public", ServeDir::new(controllers::public_dir()))
        .merge(router())
        .fallback(get(controllers::spa))
        .layer(cors::layer())
        .with_state(state)
}
