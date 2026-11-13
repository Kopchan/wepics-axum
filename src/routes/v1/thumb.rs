use axum::Router;
use axum::routing::get;

use crate::controllers;
use crate::state::AppState;

/// Generated previews with signed links, mounted under `/thumb/v1`.
pub fn router() -> Router<AppState> {
    Router::new().route(
        "/{album_hash}/{media_hash}/{spec}",
        get(controllers::v1::get_thumb),
    )
}
