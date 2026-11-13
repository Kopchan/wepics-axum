//! Query parsing shared by the v1 endpoints.

use wepics_axum::controllers::v1::types::{
    AlbumSortField, MediaSortField, parse_album_sort, parse_media_sort, parse_media_types,
    parse_preview_count,
};
use wepics_axum::models::MediaType;

#[test]
fn sort_arrays_accept_reverse_prefixes() {
    let media = parse_media_sort(Some("date,-size,name")).unwrap();
    assert_eq!(media[0].field, MediaSortField::Date);
    assert!(!media[0].descending);
    assert_eq!(media[1].field, MediaSortField::Size);
    assert!(media[1].descending);
    assert_eq!(media[2].field, MediaSortField::Name);
    assert!(!media[2].descending);

    let albums = parse_album_sort(Some("level,-created")).unwrap();
    assert_eq!(albums[0].field, AlbumSortField::Level);
    assert!(!albums[0].descending);
    assert_eq!(albums[1].field, AlbumSortField::Created);
    assert!(albums[1].descending);
}

#[test]
fn empty_sort_arrays_fall_back_to_name() {
    for sorts in [
        parse_media_sort(None).unwrap(),
        parse_media_sort(Some("")).unwrap(),
        parse_media_sort(Some(" , ")).unwrap(),
    ] {
        assert_eq!(sorts.len(), 1);
        assert_eq!(sorts[0].field, MediaSortField::Name);
        assert!(!sorts[0].descending);
    }

    for sorts in [
        parse_album_sort(None).unwrap(),
        parse_album_sort(Some("")).unwrap(),
    ] {
        assert_eq!(sorts.len(), 1);
        assert_eq!(sorts[0].field, AlbumSortField::Name);
    }
}

#[test]
fn unknown_sort_keys_are_rejected() {
    assert!(parse_media_sort(Some("bogus")).is_err());
    assert!(parse_media_sort(Some("--name")).is_err());
    assert!(parse_album_sort(Some("bogus")).is_err());
    assert!(parse_album_sort(Some("--name")).is_err());
}

#[test]
fn media_types_include_vectors_and_ignore_duplicates() {
    assert_eq!(
        parse_media_types(Some("vec,img,VECTOR,svg")).unwrap(),
        vec![MediaType::Vector, MediaType::Image]
    );
    assert!(parse_media_types(Some("bogus")).is_err());
    assert!(parse_media_types(Some("img,bogus")).is_err());
}

#[test]
fn preview_counts_are_bounded() {
    assert_eq!(parse_preview_count(None, 4).unwrap(), 4);
    assert_eq!(parse_preview_count(Some(0), 4).unwrap(), 0);
    assert_eq!(parse_preview_count(Some(200), 4).unwrap(), 200);
    assert!(parse_preview_count(Some(201), 4).is_err());
}
