use axum_server::tls_rustls::RustlsConfig;
use std::path::Path;

use super::AppConfig;

/// Loads the configured certificate pair, generating a self-signed one at the
/// default location when it is missing.
///
/// Paths come from [`AppConfig::tls_paths`]: custom `TLS_CERT`/`TLS_KEY` are
/// used only when both files exist; otherwise the pair under
/// `data/cert/{cert,key}.pem` is used and generated on first start.
pub async fn load(cfg: &AppConfig) -> Result<RustlsConfig, Box<dyn std::error::Error>> {
    if cfg.tls_custom_paths_missing() {
        eprintln!(
            "TLS:      TLS_CERT/TLS_KEY are set but not readable, falling back to {}",
            cfg.default_tls_cert().display()
        );
    }

    let (cert_path, key_path) = cfg.tls_paths();

    if cert_path.is_file() && key_path.is_file() {
        println!("TLS:      loading certificate from {}", cert_path.display());
        return Ok(RustlsConfig::from_pem_file(&cert_path, &key_path).await?);
    }

    if let Some(dir) = cert_path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    println!(
        "TLS:      generating self-signed certificate in {}",
        parent(&cert_path).display()
    );

    let subject_alt_names = vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
    ];
    let certified_key = rcgen::generate_simple_self_signed(subject_alt_names)?;

    let cert_pem = certified_key.cert.pem();
    let key_pem = certified_key.key_pair.serialize_pem();

    tokio::fs::write(&cert_path, &cert_pem).await?;
    tokio::fs::write(&key_path, &key_pem).await?;

    Ok(RustlsConfig::from_pem(cert_pem.into_bytes(), key_pem.into_bytes()).await?)
}

fn parent(path: &Path) -> &Path {
    path.parent().unwrap_or(path)
}
