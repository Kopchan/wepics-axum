//! Short opaque URL hashes for albums and media.
//!
//! Album hashes are random UUID v4 values encoded in Base62, which keeps them
//! 22 characters long instead of the 36 characters of the canonical UUID form.
//! Media hashes are XXH3-64 digests encoded in URL-safe Base64 (11 characters).

use std::io::Read;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use xxhash_rust::xxh3::Xxh3;

/// Base62 alphabet: `0-9`, `A-Z`, `a-z`.
const ALPHABET: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// Characters in a Base62-encoded 128-bit value.
pub const ALBUM_HASH_LEN: usize = 22;

/// Characters in a URL-safe Base64-encoded 64-bit digest.
pub const MEDIA_HASH_LEN: usize = 11;

/// Generates a random UUID v4 and returns its Base62 form.
pub fn album_hash() -> String {
    let mut bytes = [0_u8; 16];
    // A failing system RNG is unrecoverable; SQLite used to be the random source.
    getrandom::fill(&mut bytes).expect("system RNG is unavailable");

    // RFC 4122 version 4 and variant bits.
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;

    base62(u128::from_be_bytes(bytes))
}

/// XXH3-64 of a file's name and contents, encoded in URL-safe Base64.
///
/// The name is part of the input so two copies of the same file in one album
/// keep distinct hashes: media URLs are album-scoped, and the client uses the
/// hash as a list key. Including the contents makes the hash change when a file
/// is replaced in place.
pub fn media_hash(path: &Path) -> std::io::Result<String> {
    let mut hasher = Xxh3::new();
    if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
        hasher.update(name.as_bytes());
    }

    let mut file = std::fs::File::open(path)?;
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    Ok(URL_SAFE_NO_PAD.encode(hasher.digest().to_be_bytes()))
}

/// Encodes a 128-bit value as exactly [`ALBUM_HASH_LEN`] Base62 characters.
fn base62(mut value: u128) -> String {
    let mut out = [b'0'; ALBUM_HASH_LEN];
    let mut index = ALBUM_HASH_LEN;

    while value > 0 {
        index -= 1;
        out[index] = ALPHABET[(value % 62) as usize];
        value /= 62;
    }

    // Every byte comes from `ALPHABET`, so this is valid UTF-8.
    String::from_utf8(out.to_vec()).expect("base62 alphabet is ASCII")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn album_hash_is_22_base62_chars() {
        let hash = album_hash();
        assert_eq!(hash.len(), ALBUM_HASH_LEN);
        assert!(hash.bytes().all(|byte| ALPHABET.contains(&byte)));
    }

    #[test]
    fn album_hashes_are_random() {
        assert_ne!(album_hash(), album_hash());
    }

    #[test]
    fn media_hash_is_11_url_safe_chars() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.jpg");
        std::fs::write(&path, b"hello").unwrap();

        let hash = media_hash(&path).unwrap();
        assert_eq!(hash.len(), MEDIA_HASH_LEN);
        assert!(
            hash.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        );
    }

    #[test]
    fn media_hash_is_stable_and_depends_on_name_and_contents() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.jpg");
        let second = dir.path().join("second.jpg");
        std::fs::write(&first, b"same").unwrap();
        std::fs::write(&second, b"same").unwrap();

        assert_eq!(media_hash(&first).unwrap(), media_hash(&first).unwrap());
        assert_ne!(media_hash(&first).unwrap(), media_hash(&second).unwrap());

        std::fs::write(&first, b"changed").unwrap();
        assert_ne!(media_hash(&first).unwrap(), media_hash(&second).unwrap());
    }
}
