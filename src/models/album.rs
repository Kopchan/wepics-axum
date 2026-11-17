use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

/// Hash used in URLs to address the root album: `/api/albums/root`.
pub const ROOT_ALBUM_HASH: &str = "root";

/// A folder that groups media. Albums can be nested via `parent_id`.
#[derive(Debug, Clone, PartialEq, Eq, FromRow, Serialize, Deserialize)]
pub struct Album {
    pub id: i64,
    pub name: String,
    pub hash: String,
    /// `None` for root albums.
    pub parent_id: Option<i64>,
    /// Directory relative to the media root, `/`-separated; empty for root.
    ///
    /// This is the folder an album was discovered in, so it stays stable when
    /// the album is renamed or its URL hash changes. Internal.
    #[serde(skip_serializing, default)]
    pub path: String,
    /// Canonical URL name; `None` until an admin sets one.
    pub alias: Option<String>,
    /// JSON array of former aliases. Internal; the client sees `old_aliases`.
    #[serde(skip_serializing, default)]
    pub old_aliases: String,
    /// Age-rating level, matching the client's rating presets.
    pub rating_level: Option<i64>,
    /// Manual position used by `asort=order`; `0` means "not set".
    pub order_level: i64,
    /// `None` inherits visibility, `Some(false)` hides, `Some(true)` allows.
    pub guest_allow: Option<bool>,
    /// Natural-sort key derived from `name`; internal, never sent to clients.
    ///
    /// Writers must call [`crate::models::natural_sort_key`] and store the
    /// result here: `ORDER BY sort_name` is what makes `album2` sort before
    /// `album10`.
    #[serde(skip_serializing, default)]
    pub sort_name: String,
    /// When the album's contents were last scanned; `None` until indexed.
    pub indexed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
