use axum::Router;
use axum::routing::get;

use crate::controllers;
use crate::state::AppState;

/// JSON API, mounted under `/api`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/albums/{hash}", get(controllers::get_album))
        .route("/albums/{hash}/media", get(controllers::list_media))
        .route("/albums/{hash}/child", get(controllers::list_children))
        .route(
            "/albums/{hash}/index",
            get(controllers::start_index).post(controllers::start_index),
        )
        .route(
            "/albums/{hash}/index/events",
            get(controllers::index_events),
        )
}
