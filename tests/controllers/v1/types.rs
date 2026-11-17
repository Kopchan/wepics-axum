//! Query parsing shared by the v1 endpoints.

use wepics_axum::controllers::v1::types::{
    AlbumSortField, Limit, MediaSortField, parse_album_sort, parse_layer_bools, parse_layer_limits,
    parse_limit, parse_media_sort, parse_media_types,
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

    let albums = parse_album_sort(Some("order,-created")).unwrap();
    assert_eq!(albums[0].field, AlbumSortField::Order);
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
fn random_sort_accepts_seed() {
    let media = parse_media_sort(Some("random:42")).unwrap();
    assert_eq!(media[0].field, MediaSortField::Random);
    assert_eq!(media[0].seed, Some(42));
    assert!(parse_media_sort(Some("random:abc")).is_err());

    let albums = parse_album_sort(Some("-random:7")).unwrap();
    assert_eq!(albums[0].field, AlbumSortField::Random);
    assert!(albums[0].descending);
    assert_eq!(albums[0].seed, Some(7));
}

#[test]
fn media_types_include_vectors_and_ignore_duplicates() {
    assert_eq!(
        parse_media_types(Some("vec,img,VECTOR,svg")).unwrap(),
        vec![MediaType::Vector, MediaType::Image]
    );
    assert_eq!(
        parse_media_types(Some("ani,anim")).unwrap(),
        vec![MediaType::Animated]
    );
    assert!(parse_media_types(Some("bogus")).is_err());
    assert!(parse_media_types(Some("img,bogus")).is_err());
}

#[test]
fn limits_accept_a_star() {
    assert_eq!(
        parse_limit(None, Limit::Fixed(30)).unwrap(),
        Limit::Fixed(30)
    );
    assert_eq!(
        parse_limit(Some("*"), Limit::Fixed(30)).unwrap(),
        Limit::All
    );
    assert_eq!(
        parse_limit(Some("50"), Limit::Fixed(30)).unwrap(),
        Limit::Fixed(50)
    );
    assert!(parse_limit(Some("x"), Limit::Fixed(30)).is_err());
}

#[test]
fn layer_limits_are_slash_separated() {
    assert_eq!(
        parse_layer_limits(Some("0/30/4"), &[]).unwrap(),
        vec![Limit::Fixed(0), Limit::Fixed(30), Limit::Fixed(4)]
    );
    assert_eq!(
        parse_layer_limits(Some("*/*"), &[]).unwrap(),
        vec![Limit::All, Limit::All]
    );
    assert_eq!(
        parse_layer_limits(None, &[Limit::Fixed(7)]).unwrap(),
        vec![Limit::Fixed(7)]
    );
    assert!(parse_layer_limits(Some("201"), &[]).is_err());
    assert!(parse_layer_limits(Some("nope"), &[]).is_err());
}

#[test]
fn nested_flags_are_boolean_lists() {
    assert_eq!(parse_layer_bools(Some("-/+")), vec![false, true]);
    assert_eq!(parse_layer_bools(Some("+")), vec![true]);
    assert!(parse_layer_bools(None).is_empty());
}
