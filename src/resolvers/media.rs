use crate::error::AppError;
use crate::models::{Album, Media};
use crate::state::AppState;

/// Columns selected for a media row, kept in the same shape as [`Media`].
const MEDIA_BY_HASH: &str = "SELECT id, name, hash, \"type\", album_id, width, height, \
     duration_sec, date, size, rating_level, sort_name, created_at, updated_at FROM media \
     WHERE album_id = ?1 AND hash = ?2";

/// Looks a media row up by album and media hash.
///
/// `media.hash` is the stable identity of the file; the short-lived signatures
/// used by `/thumb/v1` and `/file/v1` live in the query string instead.
pub(crate) async fn resolve_media(
    state: &AppState,
    album: &Album,
    media_hash: &str,
) -> Result<Media, AppError> {
    sqlx::query_as(MEDIA_BY_HASH)
        .bind(album.id)
        .bind(media_hash)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)
}
