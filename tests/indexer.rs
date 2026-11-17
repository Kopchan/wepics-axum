//! Indexer classification and freshness rules.

use std::path::Path;

use chrono::{Duration, Utc};
use wepics_axum::indexer::{media_type_for, needs_sync};
use wepics_axum::models::{Album, MediaType};

fn album(indexed_at: Option<chrono::DateTime<Utc>>) -> Album {
    let now = Utc::now();
    Album {
        id: 1,
        name: "album".into(),
        hash: "hash".into(),
        parent_id: None,
        path: "album".into(),
        alias: None,
        old_aliases: "[]".into(),
        rating_level: None,
        order_level: 0,
        guest_allow: None,
        sort_name: "album".into(),
        indexed_at,
        created_at: now,
        updated_at: now,
    }
}

#[test]
fn freshness_threshold_is_one_hour() {
    assert!(needs_sync(&album(None)));
    assert!(!needs_sync(&album(Some(
        Utc::now() - Duration::minutes(30)
    ))));
    assert!(needs_sync(&album(Some(Utc::now() - Duration::minutes(61)))));
}

#[test]
fn classifies_media_extensions() {
    assert_eq!(
        media_type_for(Path::new("photo.JPG")),
        Some(MediaType::Image)
    );
    assert_eq!(
        media_type_for(Path::new("clip.mp4")),
        Some(MediaType::Video)
    );
    assert_eq!(
        media_type_for(Path::new("song.flac")),
        Some(MediaType::Audio)
    );
    assert_eq!(
        media_type_for(Path::new("anim.gif")),
        Some(MediaType::Animated)
    );
    assert_eq!(
        media_type_for(Path::new("logo.svg")),
        Some(MediaType::Vector)
    );
    assert_eq!(media_type_for(Path::new("notes.txt")), None);
}
