use axum::Router;
use axum::routing::get;

use crate::controllers;
use crate::state::AppState;

/// JSON API, mounted under `/api`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/album/{hash}", get(controllers::get_album))
        .route("/album/{hash}/media", get(controllers::list_media))
        .route("/albums/{hash}/child", get(controllers::list_children))
}
