pub mod tls;
pub mod vips;
pub mod port;

use std::net::SocketAddr;

pub struct AppConfig {
  pub database_url: String,
  pub media_dir: String,
  pub cache_dir: String,
  pub http_addr: SocketAddr,
  pub https_addr: SocketAddr,
  pub tls_cert: Option<String>,
  pub tls_key: Option<String>,
}

impl AppConfig {
  pub fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
    Ok(Self {
      database_url: std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite://data/database/wepics.db".to_string()),
      media_dir:    std::env::var("MEDIA_DIR"   ).unwrap_or_else(|_| "data/media".to_string()),
      cache_dir:    std::env::var("CACHE_DIR"   ).unwrap_or_else(|_| "data/cache".to_string()),
      http_addr:    std::env::var("ADDR"        ).unwrap_or_else(|_| "127.0.0.1:33680".to_string()).parse()?,
      https_addr:   std::env::var("HTTPS_ADDR"  ).unwrap_or_else(|_| "127.0.0.1:33643".to_string()).parse()?,
      tls_cert:     std::env::var("TLS_CERT").ok(),
      tls_key:      std::env::var("TLS_KEY").ok(),
    })
  }
}

