//! Link validation shared by the thumbnail and original-file handlers.

use chrono::Utc;

use crate::error::AppError;
use crate::secure_link;
use crate::state::AppState;

use super::types::LinkQuery;

/// Current expiry used for every link issued in one response.
pub fn link_expiry() -> i64 {
    secure_link::expires_at(Utc::now().timestamp())
}

/// Extracts the token and expiry, then verifies both against the album/media
/// pair.
pub fn verify_link(
    state: &AppState,
    album_hash: &str,
    media_hash: &str,
    query: &LinkQuery,
) -> Result<(), AppError> {
    let token = query
        .hash
        .as_deref()
        .ok_or_else(|| AppError::Forbidden("missing `hash` link parameter".into()))?;
    let exp = query
        .exp
        .ok_or_else(|| AppError::Forbidden("missing `exp` link parameter".into()))?;

    if !state.verify_media(album_hash, media_hash, token, exp) {
        return Err(AppError::Forbidden("invalid or expired media link".into()));
    }

    Ok(())
}
