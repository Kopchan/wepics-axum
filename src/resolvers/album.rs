use crate::error::AppError;
use crate::models::Album;
use crate::state::AppState;

const ALBUM_SELECT: &str = "SELECT id, name, hash, parent_id, path, indexed_at, sort_name, \
     created_at, updated_at FROM albums WHERE hash = ?1";

/// Looks an album up by hash. The root album is created at startup by
/// [`AppState::initialize`], so this is a pure read.
pub(crate) async fn resolve_album(state: &AppState, hash: &str) -> Result<Album, AppError> {
    sqlx::query_as(ALBUM_SELECT)
        .bind(hash)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)
}
