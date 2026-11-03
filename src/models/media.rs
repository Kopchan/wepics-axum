use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

/// Kind of media file. Stored in SQLite as lowercase text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(rename_all = "lowercase")]
pub enum MediaType {
    Image,
    Video,
    Audio,
    Animated,
}

/// A media file that belongs to an album.
#[derive(Debug, Clone, PartialEq, FromRow, Serialize, Deserialize)]
pub struct Media {
    pub id: i64,
    pub name: String,
    pub hash: String,
    /// `type` is not a reserved word in SQLite, so it can stay as is.
    #[sqlx(rename = "type")]
    #[serde(rename = "type")]
    pub r#type: MediaType,
    pub album_id: i64,
    /// Pixels. `0` for media without dimensions (e.g. audio).
    pub width: i64,
    /// Pixels. `0` for media without dimensions (e.g. audio).
    pub height: i64,
    /// Seconds. `None` for still images.
    pub duration_sec: Option<f64>,
    /// File timestamp, used by `?sort=date`. `None` when unknown.
    pub date: Option<DateTime<Utc>>,
    /// File size in bytes, used by `?sort=size`.
    pub size: i64,
    /// Natural-sort key derived from `name`; internal, never sent to clients.
    #[serde(skip_serializing, default)]
    pub sort_name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Builds the natural-sort key stored in `media.sort_name`.
///
/// Every run of ASCII digits is zero-padded to ten digits and the
/// rest of the name is lowercased, so plain lexicographic ordering yields
/// natural ordering: `img2` < `img10`.
pub fn natural_sort_key(name: &str) -> String {
    const NUMBER_WIDTH: usize = 10;

    let mut key = String::with_capacity(name.len() + 8);
    let mut digits = String::new();

    for ch in name.chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
        } else {
            flush_number(&mut digits, NUMBER_WIDTH, &mut key);
            key.extend(ch.to_lowercase());
        }
    }
    flush_number(&mut digits, NUMBER_WIDTH, &mut key);

    key
}

fn flush_number(digits: &mut String, width: usize, out: &mut String) {
    if digits.is_empty() {
        return;
    }

    let significant = digits.trim_start_matches('0');
    let significant = if significant.is_empty() {
        "0"
    } else {
        significant
    };
    let pad = width.saturating_sub(significant.len());

    out.push_str(&"0".repeat(pad));
    out.push_str(significant);
    digits.clear();
}

#[cfg(test)]
mod tests {
    use super::natural_sort_key;

    #[test]
    fn pads_numbers_and_lowercases() {
        assert_eq!(natural_sort_key("Img 2.JPG"), "img 0000000002.jpg");
        assert_eq!(natural_sort_key("img0007"), "img0000000007");
        assert_eq!(natural_sort_key("no digits"), "no digits");
    }

    #[test]
    fn sorts_naturally() {
        let mut names = vec!["img10.jpg", "img2.jpg", "img1.jpg", "IMG20.jpg"];

        names.sort_by_key(|name| natural_sort_key(name));

        assert_eq!(
            names,
            vec!["img1.jpg", "img2.jpg", "img10.jpg", "IMG20.jpg"]
        );
    }
}
