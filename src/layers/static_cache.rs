//! Persistent compression cache for the static frontend.
//!
//! Compressible files from `PUBLIC_DIR` are compressed to
//! `CACHE_DIR/public/<mirrored path>.br|.gz`:
//!
//! * at startup, so the first visitor does not pay for compression;
//! * lazily, when a file changed or a new one appeared.
//!
//! The in-memory index only holds metadata; the compressed bytes stay on disk,
//! so a restart reuses everything that is still fresh. Requests that do not
//! accept Brotli/Gzip, binary files and dynamic responses fall through to the
//! regular stack.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::{
    ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, ETAG, IF_MODIFIED_SINCE,
    IF_NONE_MATCH, LAST_MODIFIED, VARY,
};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use dashmap::DashMap;
use flate2::{Compression, write::GzEncoder};

use crate::config::AppConfig;

/// Below this size the compression overhead is not worth it.
const MIN_COMPRESS_BYTES: u64 = 512;
/// Refuse to load huge files into memory on cache misses.
const MAX_SOURCE_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Encoding {
    Br,
    Gzip,
}

impl Encoding {
    const ALL: [Encoding; 2] = [Encoding::Br, Encoding::Gzip];

    fn extension(self) -> &'static str {
        match self {
            Encoding::Br => "br",
            Encoding::Gzip => "gz",
        }
    }

    fn header(self) -> HeaderValue {
        match self {
            Encoding::Br => HeaderValue::from_static("br"),
            Encoding::Gzip => HeaderValue::from_static("gzip"),
        }
    }

    fn compress(self, data: &[u8]) -> std::io::Result<Vec<u8>> {
        match self {
            Encoding::Br => {
                let mut out = Vec::new();
                // Quality 5 is the usual balance between ratio and CPU time.
                let mut reader = brotli::CompressorReader::new(data, 4096, 5, 22);
                reader.read_to_end(&mut out)?;
                Ok(out)
            }
            Encoding::Gzip => {
                let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
                encoder.write_all(data)?;
                Ok(encoder.finish()?)
            }
        }
    }
}

/// Metadata of one compressed representation.
#[derive(Clone)]
struct Entry {
    source_len: u64,
    source_modified: SystemTime,
    encoded_len: u64,
    content_type: HeaderValue,
    etag: HeaderValue,
    last_modified: Option<HeaderValue>,
}

#[derive(Clone)]
pub struct StaticCompressionCache {
    public_dir: PathBuf,
    compressed_dir: PathBuf,
    entries: Arc<DashMap<(PathBuf, Encoding), Entry>>,
}

impl StaticCompressionCache {
    pub fn new(public_dir: impl Into<PathBuf>, compressed_dir: impl Into<PathBuf>) -> Self {
        Self {
            public_dir: public_dir.into(),
            compressed_dir: compressed_dir.into(),
            entries: Arc::new(DashMap::new()),
        }
    }

    /// Builds the cache from the application config.
    pub fn from_config(config: &AppConfig) -> Self {
        Self::new(&config.public_dir, config.compressed_public_dir())
    }

    fn compressed_path(&self, relative: &Path, encoding: Encoding) -> PathBuf {
        compressed_path(&self.compressed_dir, relative, encoding)
    }

    /// Returns fresh metadata for one representation, compressing the source
    /// first when the cached file is missing or older than the source.
    async fn ensure(
        &self,
        relative: &Path,
        encoding: Encoding,
        source: &Path,
        source_metadata: &std::fs::Metadata,
    ) -> Option<Entry> {
        let source_len = source_metadata.len();
        let Ok(source_modified) = source_metadata.modified() else {
            return None;
        };
        let key = (relative.to_path_buf(), encoding);

        if let Some(entry) = self.entries.get(&key)
            && entry.source_len == source_len
            && entry.source_modified == source_modified
        {
            return Some(entry.clone());
        }

        let destination = self.compressed_path(relative, encoding);
        let fresh = tokio::fs::metadata(&destination)
            .await
            .ok()
            .map(|metadata| {
                metadata.len() > 0
                    && metadata
                        .modified()
                        .map(|modified| modified >= source_modified)
                        .unwrap_or(false)
            })
            .unwrap_or(false);

        if !fresh {
            let source = source.to_path_buf();
            let target = destination.clone();
            match tokio::task::spawn_blocking(move || compress_to_file(&source, &target, encoding))
                .await
            {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    eprintln!("static cache: {}: {err}", destination.display());
                    return None;
                }
                Err(err) => {
                    eprintln!("static cache: {}: {err}", destination.display());
                    return None;
                }
            }
        }

        let encoded_len = tokio::fs::metadata(&destination).await.ok()?.len();
        let entry = Entry {
            source_len,
            source_modified,
            encoded_len,
            content_type: content_type(relative),
            etag: etag(source_len, source_modified, encoding),
            last_modified: HeaderValue::from_str(&httpdate::fmt_http_date(source_modified)).ok(),
        };

        self.entries.insert(key, entry.clone());
        Some(entry)
    }
}

/// Precompresses the whole frontend, skipping files whose cached copy is still
/// newer than the source. Returns the number of files written.
pub async fn precompress_public(config: Arc<AppConfig>) -> std::io::Result<usize> {
    precompress(&config.public_dir, &config.compressed_public_dir()).await
}

pub async fn precompress(public_dir: &Path, compressed_dir: &Path) -> std::io::Result<usize> {
    let public_dir = public_dir.to_path_buf();
    let compressed_dir = compressed_dir.to_path_buf();

    tokio::task::spawn_blocking(move || walk_and_compress(&public_dir, &compressed_dir))
        .await
        .map_err(std::io::Error::other)?
}

fn walk_and_compress(public_dir: &Path, compressed_dir: &Path) -> std::io::Result<usize> {
    std::fs::create_dir_all(compressed_dir)?;

    if !public_dir.is_dir() {
        eprintln!(
            "static cache: {} is missing, nothing to precompress",
            public_dir.display()
        );
        return Ok(0);
    }

    let mut written = 0;
    let mut stack = vec![public_dir.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) => {
                eprintln!("static cache: {}: {err}", dir.display());
                continue;
            }
        };

        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    eprintln!("static cache: {}: {err}", dir.display());
                    continue;
                }
            };
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(err) => {
                    eprintln!("static cache: {}: {err}", path.display());
                    continue;
                }
            };

            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if !file_type.is_file() {
                continue;
            }

            let Ok(relative) = path.strip_prefix(public_dir) else {
                continue;
            };
            if !is_compressible(relative) {
                continue;
            }

            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.len() < MIN_COMPRESS_BYTES || metadata.len() > MAX_SOURCE_BYTES {
                continue;
            }
            let Ok(modified) = metadata.modified() else {
                continue;
            };

            for encoding in Encoding::ALL {
                let destination = compressed_path(compressed_dir, relative, encoding);
                if compressed_is_fresh(&destination, modified) {
                    continue;
                }
                match compress_to_file(&path, &destination, encoding) {
                    Ok(()) => written += 1,
                    Err(err) => eprintln!("static cache: {}: {err}", path.display()),
                }
            }
        }
    }

    Ok(written)
}

fn compressed_is_fresh(destination: &Path, source_modified: SystemTime) -> bool {
    std::fs::metadata(destination)
        .map(|metadata| {
            metadata.len() > 0
                && metadata
                    .modified()
                    .map(|modified| modified >= source_modified)
                    .unwrap_or(false)
        })
        .unwrap_or(false)
}

fn compress_to_file(source: &Path, destination: &Path, encoding: Encoding) -> std::io::Result<()> {
    let metadata = std::fs::metadata(source)?;
    if metadata.len() > MAX_SOURCE_BYTES {
        return Err(std::io::Error::other("file is too large to compress"));
    }

    let data = std::fs::read(source)?;
    let compressed = encoding.compress(&data)?;

    if let Some(dir) = destination.parent() {
        std::fs::create_dir_all(dir)?;
    }

    // Write next to the target and rename, so a reader never sees a partial
    // file after an interrupted write. `fs::rename` replaces on Windows too.
    let temporary = temporary_path(destination);
    std::fs::write(&temporary, &compressed)?;
    if let Err(err) = std::fs::rename(&temporary, destination) {
        let _ = std::fs::remove_file(&temporary);
        return Err(err);
    }

    Ok(())
}

fn temporary_path(destination: &Path) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let name = destination
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();

    destination.with_file_name(format!(".{name}.{}.{stamp}.tmp", std::process::id()))
}

fn compressed_path(root: &Path, relative: &Path, encoding: Encoding) -> PathBuf {
    let mut destination = root.join(relative);
    let mut name = destination
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    name.push(".");
    name.push(encoding.extension());
    destination.set_file_name(name);
    destination
}

/// Middleware: serve the precompressed copy when it exists (or can be made)
/// and the client accepts the encoding.
pub async fn compress_and_cache_static(
    State(cache): State<StaticCompressionCache>,
    req: Request,
    next: Next,
) -> Response {
    let is_head = req.method() == Method::HEAD;
    if !is_head && req.method() != Method::GET {
        return next.run(req).await;
    }

    let Some(encoding) = negotiate_encoding(req.headers()) else {
        return next.run(req).await;
    };
    let Some(relative) = resolve_relative(req.uri().path()) else {
        return next.run(req).await;
    };
    if !is_compressible(&relative) {
        return next.run(req).await;
    }

    let source = cache.public_dir.join(&relative);
    let Ok(source_metadata) = tokio::fs::metadata(&source).await else {
        return next.run(req).await;
    };
    if !source_metadata.is_file() || source_metadata.len() < MIN_COMPRESS_BYTES {
        return next.run(req).await;
    }

    let Some(entry) = cache
        .ensure(&relative, encoding, &source, &source_metadata)
        .await
    else {
        return next.run(req).await;
    };

    if is_not_modified(req.headers(), &entry) {
        return not_modified(&entry);
    }

    let destination = cache.compressed_path(&relative, encoding);
    let Ok(bytes) = tokio::fs::read(&destination).await else {
        return next.run(req).await;
    };

    compressed_response(&entry, encoding, if is_head { Vec::new() } else { bytes })
}

fn compressed_response(entry: &Entry, encoding: Encoding, body: Vec<u8>) -> Response {
    let mut response = Response::new(Body::from(body));
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, entry.content_type.clone());
    headers.insert(CONTENT_ENCODING, encoding.header());
    headers.insert(VARY, HeaderValue::from_static("Accept-Encoding"));
    headers.insert(CONTENT_LENGTH, HeaderValue::from(entry.encoded_len));
    headers.insert(ETAG, entry.etag.clone());
    if let Some(last_modified) = &entry.last_modified {
        headers.insert(LAST_MODIFIED, last_modified.clone());
    }

    response
}

fn not_modified(entry: &Entry) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::NOT_MODIFIED;

    let headers = response.headers_mut();
    headers.insert(ETAG, entry.etag.clone());
    headers.insert(VARY, HeaderValue::from_static("Accept-Encoding"));
    if let Some(last_modified) = &entry.last_modified {
        headers.insert(LAST_MODIFIED, last_modified.clone());
    }

    response
}

fn negotiate_encoding(headers: &HeaderMap) -> Option<Encoding> {
    let value = headers.get(ACCEPT_ENCODING)?.to_str().ok()?;
    let mut br = false;
    let mut gzip = false;

    for part in value.split(',') {
        let mut pieces = part.split(';');
        let token = pieces
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let disabled = pieces.any(|parameter| {
            parameter
                .trim()
                .replace(' ', "")
                .eq_ignore_ascii_case("q=0")
        });

        if disabled {
            continue;
        }

        match token.as_str() {
            "br" => br = true,
            "gzip" | "x-gzip" | "*" => gzip = true,
            _ => {}
        }
    }

    if br {
        Some(Encoding::Br)
    } else if gzip {
        Some(Encoding::Gzip)
    } else {
        None
    }
}

fn resolve_relative(uri_path: &str) -> Option<PathBuf> {
    let relative = uri_path
        .strip_prefix("/public/")
        .or_else(|| uri_path.strip_prefix('/'))
        .unwrap_or(uri_path);

    if relative.is_empty() {
        return None;
    }

    let relative = Path::new(relative);
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return None;
    }

    Some(relative.to_path_buf())
}

fn is_compressible(path: &Path) -> bool {
    let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
        return false;
    };

    matches!(
        extension.to_ascii_lowercase().as_str(),
        "html"
            | "htm"
            | "css"
            | "js"
            | "mjs"
            | "cjs"
            | "json"
            | "map"
            | "webmanifest"
            | "svg"
            | "txt"
            | "xml"
            | "wasm"
            | "csv"
    )
}

fn content_type(path: &Path) -> HeaderValue {
    let mime = mime_guess::from_path(path)
        .first_or_octet_stream()
        .to_string();
    let value = if mime.starts_with("text/")
        || mime.contains("javascript")
        || mime.contains("json")
        || mime.contains("xml")
        || mime.contains("svg")
    {
        format!("{mime}; charset=utf-8")
    } else {
        mime
    };

    HeaderValue::from_str(&value)
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"))
}

fn etag(source_len: u64, modified: SystemTime, encoding: Encoding) -> HeaderValue {
    let nanos = modified
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let value = format!("\"{source_len:x}-{nanos:x}-{}\"", encoding.extension());

    HeaderValue::from_str(&value).unwrap_or_else(|_| HeaderValue::from_static("\"static\""))
}

fn is_not_modified(headers: &HeaderMap, entry: &Entry) -> bool {
    if let Some(value) = headers
        .get(IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
    {
        let etag = entry.etag.to_str().unwrap_or_default();
        return value.split(',').any(|candidate| {
            let candidate = candidate.trim();
            candidate == "*" || candidate == etag || candidate.strip_prefix("W/") == Some(etag)
        });
    }

    if let Some(value) = headers
        .get(IF_MODIFIED_SINCE)
        .and_then(|value| value.to_str().ok())
        && let Ok(since) = httpdate::parse_http_date(value)
        && entry.source_modified <= since + Duration::from_secs(1)
    {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_text_like_files_are_compressed() {
        for name in [
            "app.js",
            "style.CSS",
            "index.html",
            "x.svg",
            "data.json",
            "m.webmanifest",
        ] {
            assert!(is_compressible(Path::new(name)), "{name}");
        }
        for name in [
            "photo.png",
            "font.woff2",
            "clip.mp4",
            "favicon.ico",
            "noext",
        ] {
            assert!(!is_compressible(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn negotiation_prefers_brotli_and_honours_q_zero() {
        let headers = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(ACCEPT_ENCODING, HeaderValue::from_str(value).unwrap());
            headers
        };

        assert_eq!(negotiate_encoding(&headers("gzip, br")), Some(Encoding::Br));
        assert_eq!(negotiate_encoding(&headers("gzip")), Some(Encoding::Gzip));
        assert_eq!(
            negotiate_encoding(&headers("br;q=0, gzip")),
            Some(Encoding::Gzip)
        );
        assert_eq!(negotiate_encoding(&headers("identity")), None);
    }

    #[test]
    fn path_traversal_is_rejected() {
        assert_eq!(
            resolve_relative("/assets/app.js"),
            Some(PathBuf::from("assets/app.js"))
        );
        assert_eq!(
            resolve_relative("/public/assets/app.js"),
            Some(PathBuf::from("assets/app.js"))
        );
        assert_eq!(resolve_relative("/../secret"), None);
        assert_eq!(resolve_relative("/assets/../../secret"), None);
        assert_eq!(resolve_relative("/"), None);
    }
}
