use axum::Router;
use axum::routing::get;

use crate::controllers;
use crate::state::AppState;

/// Streaming video previews with signed links, mounted under `/transc/v2`.
pub fn router() -> Router<AppState> {
    Router::new().route(
        "/{album_hash}/{media_hash}/{spec}",
        get(controllers::v1::get_transcode),
    )
}
