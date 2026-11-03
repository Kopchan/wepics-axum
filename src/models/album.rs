use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

/// Hash used in URLs to address the root album: `/api/album/root`.
pub const ROOT_ALBUM_HASH: &str = "root";

/// A folder that groups media. Albums can be nested via `parent_id`.
#[derive(Debug, Clone, PartialEq, Eq, FromRow, Serialize, Deserialize)]
pub struct Album {
    pub id: i64,
    pub name: String,
    pub hash: String,
    /// `None` for root albums.
    pub parent_id: Option<i64>,
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
