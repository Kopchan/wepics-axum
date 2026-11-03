use tokio::net::TcpListener;

use wepics_axum::routes;
use wepics_axum::state::AppState;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let database_url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite://data/wepics.db".to_string());
    let media_dir = std::env::var("MEDIA_DIR").unwrap_or_else(|_| "data/media".to_string());

    let state = AppState::connect(&database_url, media_dir).await?;
    let app = routes::app(state);

    let addr = std::env::var("ADDR").unwrap_or_else(|_| "127.0.0.1:3000".to_string());
    let listener = TcpListener::bind(&addr).await?;
    println!("Listening on http://{addr}");
    println!("Database: {database_url}");

    axum::serve(listener, app).await?;
    Ok(())
}
