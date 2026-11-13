//! Short-lived media link signatures.

use wepics_axum::secure_link::{TOKEN_TTL_SECONDS, expires_at, token, verify};

const SECRET: &[u8] = b"test secret";

#[test]
fn token_is_stable_and_covers_every_field() {
    let first = token(SECRET, "album", "media", 1000);
    assert_eq!(first, token(SECRET, "album", "media", 1000));
    assert_ne!(first, token(SECRET, "other", "media", 1000));
    assert_ne!(first, token(SECRET, "album", "other", 1000));
    assert_ne!(first, token(SECRET, "album", "media", 1001));
    assert_ne!(first, token(b"other secret", "album", "media", 1000));
}

#[test]
fn verification_checks_token_and_expiry() {
    let exp = expires_at(10_000);
    assert_eq!(exp, 10_000 + TOKEN_TTL_SECONDS);

    let good = token(SECRET, "album", "media", exp);
    assert!(verify(SECRET, "album", "media", &good, exp, 10_000));
    assert!(!verify(SECRET, "album", "media", &good, exp, exp + 1));
    assert!(!verify(SECRET, "album", "media", "bad", exp, 10_000));
    assert!(!verify(SECRET, "other", "media", &good, exp, 10_000));
}
