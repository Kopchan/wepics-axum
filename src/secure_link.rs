//! Short-lived signatures for media links.
//!
//! The secret salt is generated once per process start, so links stop working
//! after a restart. A signature covers the album hash, the media hash and the
//! expiry timestamp; the expiry is also sent alongside it and checked on every
//! request.

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Media links are valid for six hours.
pub const TOKEN_TTL_SECONDS: i64 = 6 * 60 * 60;

type HmacSha256 = Hmac<Sha256>;

/// Unix timestamp of a link issued at `now`.
pub fn expires_at(now: i64) -> i64 {
    now + TOKEN_TTL_SECONDS
}

/// Hex-encoded HMAC-SHA256 of `album_hash:media_hash:exp`.
pub fn token(secret: &[u8], album_hash: &str, media_hash: &str, exp: i64) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts keys of any length");
    mac.update(format!("{album_hash}:{media_hash}:{exp}").as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Checks a link token and its expiry.
pub fn verify(
    secret: &[u8],
    album_hash: &str,
    media_hash: &str,
    token_hex: &str,
    exp: i64,
    now: i64,
) -> bool {
    if exp < now {
        return false;
    }

    let expected = token(secret, album_hash, media_hash, exp);
    constant_time_eq(expected.as_bytes(), token_hex.as_bytes())
}

/// Compares two byte strings without an early exit, so a wrong token does not
/// reveal how many leading characters were correct.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }

    let mut difference = 0_u8;
    for (left, right) in left.iter().zip(right) {
        difference |= left ^ right;
    }

    difference == 0
}

#[cfg(test)]
mod tests {
    use super::{TOKEN_TTL_SECONDS, expires_at, token, verify};

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
}
