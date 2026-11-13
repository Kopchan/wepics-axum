use axum::Router;
use axum::routing::get;

use crate::controllers;
use crate::state::AppState;

/// Original files with signed links, mounted under `/file/v1`.
pub fn router() -> Router<AppState> {
    Router::new().route("/{album_hash}/{media_hash}", get(controllers::v1::get_file))
}
