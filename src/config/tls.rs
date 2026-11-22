use axum_server::tls_rustls::RustlsConfig;
use std::path::PathBuf;

pub async fn load_or_generate(
  cert_env: Option<String>,
  key_env: Option<String>,
) -> Result<RustlsConfig, Box<dyn std::error::Error>> {
  if let (Some(cert), Some(key)) = (cert_env, key_env) {
    println!("TLS: loading certificate from {cert}");
    return Ok(RustlsConfig::from_pem_file(cert, key).await?);
  }

  let cert_dir = PathBuf::from("data/cert");
  let cert_path = cert_dir.join("cert.pem");
  let key_path = cert_dir.join("key.pem");

  if cert_path.exists() && key_path.exists() {
    println!("TLS: loading existing certificate from {}", cert_dir.display());
    return Ok(RustlsConfig::from_pem_file(cert_path, key_path).await?);
  }

  println!("TLS: generating self-signed certificate in {}...", cert_dir.display());
  tokio::fs::create_dir_all(&cert_dir).await?;

  let subject_alt_names = vec!["localhost".to_string(), "127.0.0.1".to_string()];
  let certified_key = rcgen::generate_simple_self_signed(subject_alt_names)?;

  let cert_pem = certified_key.cert.pem();
  let key_pem = certified_key.key_pair.serialize_pem();

  tokio::fs::write(&cert_path, &cert_pem).await?;
  tokio::fs::write(&key_path, &key_pem).await?;

  Ok(RustlsConfig::from_pem(cert_pem.into_bytes(), key_pem.into_bytes()).await?)
}
