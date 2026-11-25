use std::sync::Arc;

use wepics_axum::config::{self, AppConfig};
use wepics_axum::layers;
use wepics_axum::routes;
use wepics_axum::state::AppState;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = Arc::new(AppConfig::from_env()?);
    config::vips::ensure()?;

    let state = AppState::connect(cfg.clone()).await?;

    // Готовим сжатые копии фронтенда до открытия портов: первый посетитель
    // должен получить `.br`/`.gz` уже с диска.
    let written = layers::static_cache::precompress_public(cfg.clone()).await?;
    if written > 0 {
        println!(
            "Static:   compressed {written} files into {}",
            cfg.compressed_public_dir().display()
        );
    } else {
        println!(
            "Static:   {} is up to date",
            cfg.compressed_public_dir().display()
        );
    }

    let app = routes::app(state.clone());
    let tls_config = config::tls::load(&cfg).await?;

    // Ищем свободные порты (шаг +1, до `BIND_ATTEMPTS` попыток)
    let (std_http_listener, http_addr) =
        config::port::bind_available_port(cfg.http_addr, cfg.bind_attempts)?;
    let (std_https_listener, https_addr) =
        config::port::bind_available_port(cfg.https_addr, cfg.bind_attempts)?;

    let tokio_http_listener = tokio::net::TcpListener::from_std(std_http_listener)?;

    println!("Data:     {}", cfg.data_dir.display());
    println!("Media:    {}", cfg.media_dir.display());
    println!("Cache:    {}", cfg.cache_dir.display());
    println!("Public:   {}", cfg.public_dir.display());
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
