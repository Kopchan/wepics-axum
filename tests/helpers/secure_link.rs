//! Short-lived media link signatures.

use wepics_axum::helpers::secure_link::{
    BUCKET_SECONDS, TOKEN_TTL_SECONDS, expires_at, token, verify,
};

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
fn expiry_is_quantized_into_three_hour_buckets() {
    // A link issued at the start of a bucket lives exactly the full TTL.
    let start = BUCKET_SECONDS;
    assert_eq!(expires_at(start), start + TOKEN_TTL_SECONDS);

    // Every request within the bucket gets the same `exp`, hence the same
    // token and URL, so browsers can reuse the cached response.
    let exp = expires_at(start);
    assert_eq!(expires_at(start + BUCKET_SECONDS - 1), exp);
    assert_eq!(
        token(SECRET, "album", "media", exp),
        token(
            SECRET,
            "album",
            "media",
            expires_at(start + BUCKET_SECONDS - 1)
        )
    );

    // The next bucket issues a fresh link with a later expiry.
    assert_eq!(
        expires_at(start + BUCKET_SECONDS),
        start + TOKEN_TTL_SECONDS + BUCKET_SECONDS
    );

    // Links never live shorter than one bucket (three hours).
    for offset in [0, 1, BUCKET_SECONDS / 2, BUCKET_SECONDS - 1] {
        let now = start + offset;
        assert!(expires_at(now) - now >= BUCKET_SECONDS);
        assert!(expires_at(now) - now <= TOKEN_TTL_SECONDS);
    }
}

#[test]
fn verification_checks_token_and_expiry() {
    let exp = expires_at(10_000);

    let good = token(SECRET, "album", "media", exp);
    assert!(verify(SECRET, "album", "media", &good, exp, 10_000));
    assert!(!verify(SECRET, "album", "media", &good, exp, exp + 1));
    assert!(!verify(SECRET, "album", "media", "bad", exp, 10_000));
    assert!(!verify(SECRET, "other", "media", &good, exp, 10_000));
}
