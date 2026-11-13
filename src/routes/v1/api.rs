use axum::Router;
use axum::routing::get;

use crate::controllers;
use crate::state::AppState;

/// JSON API, mounted under `/api/v1`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/album/{hash}", get(controllers::v1::get_album))
        .route("/album/{hash}/media", get(controllers::v1::list_media))
}
