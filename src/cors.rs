//! CORS for the whole application.
//!
//! The API is public: reads are open to any origin and indexing is started with
//! `POST`. `Range` and the conditional headers browsers send for cached files
//! are allowed, and the headers needed to consume partial/cached responses are
//! exposed.

use std::time::Duration;

use axum::http::Method;
use axum::http::header::{
    ACCEPT, ACCEPT_RANGES, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG,
    IF_MODIFIED_SINCE, IF_NONE_MATCH, IF_RANGE, LAST_MODIFIED, RANGE,
};
use tower_http::cors::{Any, CorsLayer};

/// CORS policy for the public API.
pub fn layer() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::HEAD, Method::POST, Method::OPTIONS])
        .allow_headers([
            ACCEPT,
            CONTENT_TYPE,
            RANGE,
            IF_RANGE,
            IF_NONE_MATCH,
            IF_MODIFIED_SINCE,
        ])
        .expose_headers([
            ACCEPT_RANGES,
            CACHE_CONTROL,
            CONTENT_LENGTH,
            CONTENT_RANGE,
            ETAG,
            LAST_MODIFIED,
        ])
        .max_age(Duration::from_secs(86_400))
}
