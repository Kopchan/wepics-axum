//! Lookups that turn route parameters or ids into domain models.
//!
//! These are thin, reusable wrappers over `sqlx` queries: handlers ask for an
//! album or a media row by its hash instead of repeating SQL and `NotFound`
//! handling. Nothing here knows about HTTP responses or query strings.

mod album;
mod media;

pub(crate) use album::resolve_album;
pub(crate) use media::resolve_media;
