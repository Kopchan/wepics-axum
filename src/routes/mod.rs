pub mod api;
pub mod files;

use axum::Router;
use axum::middleware::from_fn;
use axum::routing::get;

use crate::controllers;
use crate::cors;
use crate::state::AppState;

/// Everything except the trivial root/health routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .nest("/api", api::router())
        .nest("/files", files::router())
}

/// Full application: root/health pages, `/api`, `/files`, and CORS for all.
pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/", get(controllers::hello))
        .route("/hello/{name}", get(controllers::hello_name))
        .route("/health", get(controllers::health))
        .merge(router())
        .layer(from_fn(cors::cors))
        .with_state(state)
}
