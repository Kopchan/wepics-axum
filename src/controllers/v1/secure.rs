//! Link validation shared by the thumbnail and original-file handlers.

use axum::http::HeaderValue;
use chrono::Utc;

use crate::error::AppError;
use crate::helpers::secure_link;
use crate::state::AppState;

use super::types::LinkQuery;

/// Current expiry used for every link issued in one response.
pub fn link_expiry() -> i64 {
    secure_link::expires_at(Utc::now().timestamp())
}

/// Extracts the token and expiry, verifies both, and returns the validated `exp`.
pub fn verify_link(
    state: &AppState,
    album_hash: &str,
    media_hash: &str,
    query: &LinkQuery,
) -> Result<i64, AppError> {
    let token = query
        .sign
        .as_deref()
        .ok_or_else(|| AppError::Forbidden("missing `sign` link parameter".into()))?;
    let exp = query
        .exp
        .ok_or_else(|| AppError::Forbidden("missing `exp` link parameter".into()))?;

    if !state.verify_media(album_hash, media_hash, token, exp) {
        return Err(AppError::Forbidden("invalid or expired media link".into()));
    }

    Ok(exp)
}

/// Builds a `Cache-Control` header matching the remaining lifetime of `exp`.
pub fn media_cache_control(exp: i64) -> HeaderValue {
    let max_age = (exp - Utc::now().timestamp()).max(0);
    if max_age == 0 {
        return HeaderValue::from_static("no-store");
    }

    HeaderValue::from_str(&format!("private, max-age={max_age}, immutable"))
        .unwrap_or_else(|_| HeaderValue::from_static("no-store"))
}
