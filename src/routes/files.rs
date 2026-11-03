use axum::Router;
use axum::routing::get;

use crate::controllers;
use crate::state::AppState;

/// Raw file downloads, mounted under `/files`.
pub fn router() -> Router<AppState> {
    Router::new().route("/{album_hash}/{media_hash}", get(controllers::get_file))
}
