use wepics_axum::config::{self, AppConfig};
use wepics_axum::routes;
use wepics_axum::state::AppState;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  let cfg = AppConfig::from_env()?;
  config::vips::ensure()?;

  let state = AppState::connect(&cfg.database_url, &cfg.media_dir, &cfg.cache_dir).await?;
  let app = routes::app(state.clone());

  let tls_config = config::tls::load_or_generate(cfg.tls_cert, cfg.tls_key).await?;

  // Ищем свободные порты (шаг +1, до 100 попыток)
  let (std_http_listener, http_addr) = config::port::bind_available_port(cfg.http_addr, 100)?;
  let (std_https_listener, https_addr) = config::port::bind_available_port(cfg.https_addr, 100)?;

  let tokio_http_listener = tokio::net::TcpListener::from_std(std_http_listener)?;

  println!("Database: {}", cfg.database_url);
  println!("Media:    {}", state.media_dir.display());
  println!("Cache:    {}", state.cache_dir.display());
  println!("HTTP:     http://{http_addr}");
  println!("HTTPS:    https://{https_addr}");

  let http_app = app.clone();
  let http_server = async move {
    axum::serve(tokio_http_listener, http_app)
      .await
      .map_err(Box::<dyn std::error::Error>::from)
  };

  let https_server = async move {
    axum_server::from_tcp_rustls(std_https_listener, tls_config)?
      .serve(app.into_make_service())
      .await
      .map_err(Box::<dyn std::error::Error>::from)
  };

  tokio::try_join!(http_server, https_server)?;

  Ok(())
}
