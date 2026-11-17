use axum::Router;
use axum::routing::get;

use crate::controllers;
use crate::state::AppState;

/// JSON API, mounted under `/api/v1`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/album/{hash}", get(controllers::v1::get_album))
        .route("/album/{hash}/childs", get(controllers::v1::list_children))
        .route("/album/{hash}/media", get(controllers::v1::list_media))
        .route(
            "/album/{hash}/media/{media}",
            get(controllers::v1::get_media),
        )
        .route(
            "/album/{hash}/index",
            get(controllers::v1::start_index).post(controllers::v1::start_index),
        )
        .route(
            "/album/{hash}/index/events",
            get(controllers::v1::index_events),
        )
}
