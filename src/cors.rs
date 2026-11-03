//! CORS for the whole application, applied once as a tower/axum middleware.
//!
//! The API is public and read-only, so any origin may read it. `Range` is
//! allowed on requests and the headers needed to consume partial responses are
//! exposed to browser JavaScript.

use axum::body::Body;
use axum::extract::Request;
use axum::http::header::{
    ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
    ACCESS_CONTROL_EXPOSE_HEADERS, ACCESS_CONTROL_MAX_AGE, ACCESS_CONTROL_REQUEST_METHOD, ORIGIN,
};
use axum::http::{HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::Response;

const ALLOWED_METHODS: &str = "GET, HEAD, OPTIONS";
const ALLOWED_HEADERS: &str = "accept, content-type, range, if-range, if-none-match";
const EXPOSED_HEADERS: &str =
    "accept-ranges, cache-control, content-length, content-range, etag, last-modified";
const MAX_AGE_SECS: &str = "86400";

/// Adds CORS headers to every response and answers preflight requests.
pub async fn cors(request: Request, next: Next) -> Response {
    if is_preflight(&request) {
        return preflight();
    }

    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    headers.insert(
        ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static(EXPOSED_HEADERS),
    );
    response
}

/// A preflight is an `OPTIONS` request that carries both `Origin` and
/// `Access-Control-Request-Method`; answering it here avoids a 405 from the
/// router, which only knows `GET`.
fn is_preflight(request: &Request) -> bool {
    request.method() == Method::OPTIONS
        && request.headers().contains_key(ORIGIN)
        && request
            .headers()
            .contains_key(ACCESS_CONTROL_REQUEST_METHOD)
}

fn preflight() -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::NO_CONTENT;

    let headers = response.headers_mut();
    headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    headers.insert(
        ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static(ALLOWED_METHODS),
    );
    headers.insert(
        ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static(ALLOWED_HEADERS),
    );
    headers.insert(
        ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static(MAX_AGE_SECS),
    );
    response
}
