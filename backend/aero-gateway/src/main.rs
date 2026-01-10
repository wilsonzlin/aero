use std::{net::SocketAddr, str::FromStr};

use aero_gateway::{build_app, GatewayConfig};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let bind_addr = std::env::var("AERO_GATEWAY_BIND_ADDR")
        .ok()
        .and_then(|v| SocketAddr::from_str(&v).ok())
        .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 8080)));

    let config = GatewayConfig::from_env();
    let app = build_app(config).await?;

    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    tracing::info!("aero-gateway listening on http://{bind_addr}");
    axum::serve(listener, app).await
}
