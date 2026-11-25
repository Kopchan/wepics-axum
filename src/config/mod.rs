//! Runtime configuration for the server.
//!
//! This is the only module that reads environment variables. Every other
//! module receives [`AppConfig`] through
//! [`AppState::config`](crate::state::AppState::config), so adding a new
//! setting means adding one field here (and, if needed, one default) instead
//! of threading a new value through handlers.
//!
//! All storage defaults hang off a single root folder, [`AppConfig::data_dir`]
//! (`DATA_DIR`, `data` by default). Moving everything is just
//! `DATA_DIR=wepics-data`:
//!
//! ```text
//! data/media              MEDIA_DIR
//! data/cache              CACHE_DIR (previews + compressed frontend)
//! data/cache/public       precompressed static files (.br / .gz)
//! data/database/wepics.db DATABASE_FILE
//! data/cert/cert.pem      TLS_CERT, generated when missing
//! data/cert/key.pem       TLS_KEY,  generated when missing
//! ```
//!
//! | Variable | Meaning | Default |
//! | --- | --- | --- |
//! | `DATA_DIR` | root for everything the server writes | `data` |
//! | `DATABASE_URL` | full SQLite URL, overrides `DATABASE_FILE` | — |
//! | `DATABASE_FILE` | SQLite file | `{DATA_DIR}/database/wepics.db` |
//! | `MEDIA_DIR` | photo/video library | `{DATA_DIR}/media` |
//! | `CACHE_DIR` | previews and compressed frontend | `{DATA_DIR}/cache` |
//! | `PUBLIC_DIR` | built frontend | `public` |
//! | `ADDR` | HTTP address | `0.0.0.0:33680` |
//! | `HTTPS_ADDR` | HTTPS address | `0.0.0.0:33643` |
//! | `TLS_CERT` / `TLS_KEY` | certificate pair, used only when both exist | `{DATA_DIR}/cert/*.pem` |
//! | `PUBLIC_URL` | canonical origin for OG tags, request host otherwise | — |
//! | `BIND_ATTEMPTS` | ports to probe when the configured one is busy | `100` |
//!
//! Relative `DATA_DIR` and `PUBLIC_DIR` defaults live beside the executable
//! in an installed build, and in the working directory during development.

pub mod port;
pub mod tls;
pub mod vips;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

const DEFAULT_DATA_DIR: &str = "data";
const DEFAULT_PUBLIC_DIR: &str = "public";
const DEFAULT_BIND_ATTEMPTS: u16 = 100;

#[derive(Debug, Clone)]
pub struct AppConfig {
    /// Root for everything the server writes (media, cache, database, certs).
    pub data_dir: PathBuf,
    /// Explicit SQLite connection string. When `None`, [`Self::database_file`]
    /// is used instead, which avoids URL-escaping issues with Windows paths.
    pub database_url: Option<String>,
    /// SQLite file used when `DATABASE_URL` is not set.
    pub database_file: PathBuf,
    pub media_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub public_dir: PathBuf,
    pub http_addr: SocketAddr,
    pub https_addr: SocketAddr,
    /// Certificate paths from the environment. They are only used when both
    /// files exist; otherwise the defaults under `data_dir/cert` are used.
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    /// Public origin for canonical/OG URLs. When unset, the request host wins.
    pub public_url: Option<String>,
    /// How many ports to probe when the configured one is busy.
    pub bind_attempts: u16,
}

impl AppConfig {
    /// Pure defaults, without touching the environment. Useful for tests.
    pub fn defaults() -> Self {
        let data_dir = PathBuf::from(DEFAULT_DATA_DIR);

        Self {
            database_url: None,
            database_file: data_dir.join("database").join("wepics.db"),
            media_dir: data_dir.join("media"),
            cache_dir: data_dir.join("cache"),
            public_dir: PathBuf::from(DEFAULT_PUBLIC_DIR),
            http_addr: SocketAddr::from(([0, 0, 0, 0], 33680)),
            https_addr: SocketAddr::from(([0, 0, 0, 0], 33643)),
            tls_cert: None,
            tls_key: None,
            public_url: None,
            bind_attempts: DEFAULT_BIND_ATTEMPTS,
            data_dir,
        }
    }

    /// Reads every supported environment variable. The only place env is read.
    ///
    /// Relative defaults (`data`, `public`) are resolved against the working
    /// directory when it holds the frontend (development), otherwise against
    /// the executable's directory (the installed layout, where the installer
    /// puts `public` next to the binary). Explicit environment paths are used
    /// as given.
    pub fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        let defaults = Self::defaults();
        let base = base_dir();
        let data_dir = env_path("DATA_DIR").unwrap_or_else(|| base.join(DEFAULT_DATA_DIR));

        Ok(Self {
            database_url: env_string("DATABASE_URL"),
            database_file: env_path("DATABASE_FILE")
                .unwrap_or_else(|| data_dir.join("database").join("wepics.db")),
            media_dir: env_path("MEDIA_DIR").unwrap_or_else(|| data_dir.join("media")),
            cache_dir: env_path("CACHE_DIR").unwrap_or_else(|| data_dir.join("cache")),
            public_dir: env_path("PUBLIC_DIR").unwrap_or_else(|| base.join(DEFAULT_PUBLIC_DIR)),
            http_addr: env_addr("ADDR", defaults.http_addr)?,
            https_addr: env_addr("HTTPS_ADDR", defaults.https_addr)?,
            tls_cert: env_path("TLS_CERT"),
            tls_key: env_path("TLS_KEY"),
            public_url: env_string("PUBLIC_URL")
                .map(|url| url.trim_end_matches('/').to_owned())
                .filter(|url| !url.is_empty()),
            bind_attempts: env_number("BIND_ATTEMPTS").unwrap_or(defaults.bind_attempts),
            data_dir,
        })
    }

    /// Directory with precompressed copies of [`Self::public_dir`].
    ///
    /// Mirrors the frontend tree: `public/assets/app.js` becomes
    /// `cache/public/assets/app.js.br` and `...app.js.gz`.
    pub fn compressed_public_dir(&self) -> PathBuf {
        self.cache_dir.join("public")
    }

    pub fn default_tls_cert(&self) -> PathBuf {
        self.data_dir.join("cert").join("cert.pem")
    }

    pub fn default_tls_key(&self) -> PathBuf {
        self.data_dir.join("cert").join("key.pem")
    }

    /// Certificate/key pair that should be served.
    ///
    /// Custom paths from the environment win only when both files exist.
    /// Otherwise the defaults under `data_dir/cert` are used (and generated by
    /// [`tls::load`] if they are missing).
    pub fn tls_paths(&self) -> (PathBuf, PathBuf) {
        match (&self.tls_cert, &self.tls_key) {
            (Some(cert), Some(key)) if cert.is_file() && key.is_file() => {
                (cert.clone(), key.clone())
            }
            _ => (self.default_tls_cert(), self.default_tls_key()),
        }
    }

    /// True when the environment points at certificates that are not there
    /// (or only one of the two paths is set).
    pub fn tls_custom_paths_missing(&self) -> bool {
        let custom = self.tls_cert.is_some() || self.tls_key.is_some();
        let usable = matches!(
            (&self.tls_cert, &self.tls_key),
            (Some(cert), Some(key)) if cert.is_file() && key.is_file()
        );

        custom && !usable
    }

    /// Root directory used to store previews.
    pub fn thumb_dir(&self) -> PathBuf {
        thumb_dir(&self.cache_dir)
    }
}

/// Root directory for generated previews, from a cache root.
pub fn thumb_dir(cache_dir: &Path) -> PathBuf {
    cache_dir.join("thumbs").join("v1")
}

fn env_string(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn env_path(name: &str) -> Option<PathBuf> {
    env_string(name).map(PathBuf::from)
}

/// Directory relative defaults hang off.
///
/// Returns an empty path (so joins stay relative) when the working directory
/// already contains the frontend, which is how `cargo run` and the tests
/// work. The installed layout instead keeps `public` beside the executable,
/// so relative defaults are anchored there.
fn base_dir() -> PathBuf {
    if Path::new(DEFAULT_PUBLIC_DIR).is_dir() {
        return PathBuf::new();
    }

    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .filter(|dir| dir.join(DEFAULT_PUBLIC_DIR).is_dir())
        .unwrap_or_default()
}

fn env_number<T: std::str::FromStr>(name: &str) -> Option<T> {
    env_string(name)?.parse().ok()
}

fn env_addr(name: &str, default: SocketAddr) -> Result<SocketAddr, Box<dyn std::error::Error>> {
    match env_string(name) {
        Some(value) => value
            .parse()
            .map_err(|err| format!("invalid {name} `{value}`: {err}").into()),
        None => Ok(default),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_share_one_data_root() {
        let config = AppConfig::defaults();

        assert_eq!(config.media_dir, PathBuf::from("data").join("media"));
        assert_eq!(config.cache_dir, PathBuf::from("data").join("cache"));
        assert_eq!(
            config.database_file,
            PathBuf::from("data").join("database").join("wepics.db")
        );
        assert_eq!(
            config.compressed_public_dir(),
            PathBuf::from("data").join("cache").join("public")
        );
        assert_eq!(
            config.default_tls_cert(),
            PathBuf::from("data").join("cert").join("cert.pem")
        );
    }

    #[test]
    fn missing_custom_certificates_fall_back_to_defaults() {
        let mut config = AppConfig::defaults();
        config.data_dir = PathBuf::from("wepics-data");
        config.tls_cert = Some(PathBuf::from("definitely/not/here/cert.pem"));
        config.tls_key = Some(PathBuf::from("definitely/not/here/key.pem"));

        let (cert, key) = config.tls_paths();
        assert_eq!(
            cert,
            PathBuf::from("wepics-data").join("cert").join("cert.pem")
        );
        assert_eq!(
            key,
            PathBuf::from("wepics-data").join("cert").join("key.pem")
        );
        assert!(config.tls_custom_paths_missing());
    }
}
