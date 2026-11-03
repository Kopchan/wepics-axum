use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let app = axum::Router::new()
        .route("/", axum::routing::get(hello))
        .route("/hello/{name}", axum::routing::get(hello_name));

    let addr = "127.0.0.1:3000";
    let listener = TcpListener::bind(addr).await?;
    println!("Listening on http://{addr}");

    axum::serve(listener, app).await?;
    Ok(())
}

async fn hello() -> &'static str {
    "Hello, World!"
}

async fn hello_name(axum::extract::Path(name): axum::extract::Path<String>) -> String {
    format!("Hello, {name}!")
}
