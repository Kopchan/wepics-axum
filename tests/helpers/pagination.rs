//! `?page=&limit=` validation.

use wepics_axum::helpers::pagination::{DEFAULT_LIMIT, MAX_LIMIT, Pagination};

#[test]
fn defaults_are_page_one_and_thirty_items() {
    let pagination = Pagination::new(None, None).unwrap();
    assert_eq!(pagination.page, 1);
    assert_eq!(pagination.limit, DEFAULT_LIMIT);
    assert_eq!(pagination.offset, 0);
}

#[test]
fn offsets_and_page_counts_are_computed() {
    let pagination = Pagination::new(Some(3), Some(10)).unwrap();
    assert_eq!(pagination.offset, 20);
    assert_eq!(pagination.pages(0), 0);
    assert_eq!(pagination.pages(1), 1);
    assert_eq!(pagination.pages(10), 1);
    assert_eq!(pagination.pages(11), 2);
    assert_eq!(pagination.pages(25), 3);
}

#[test]
fn invalid_values_are_rejected() {
    assert!(Pagination::new(Some(0), None).is_err());
    assert!(Pagination::new(None, Some(0)).is_err());
    assert!(Pagination::new(None, Some(MAX_LIMIT)).is_ok());
    assert!(Pagination::new(None, Some(MAX_LIMIT + 1)).is_err());
}
