use std::{net::SocketAddr, str::FromStr};

use aero_gateway_rs::{build_app, GatewayConfig};

fn tokio_worker_threads_from_env() -> Option<usize> {
    let raw = match std::env::var("AERO_TOKIO_WORKER_THREADS") {
        Ok(v) => v,
        Err(_) => return None,
    };
    match raw.parse::<usize>() {
        Ok(n) if n > 0 => Some(n),
        _ => {
            eprintln!(
                "warning: invalid AERO_TOKIO_WORKER_THREADS value: {raw:?} (expected positive integer); using Tokio default"
            );
            None
        }
    }
}

fn build_tokio_runtime() -> std::io::Result<tokio::runtime::Runtime> {
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    if let Some(n) = tokio_worker_threads_from_env() {
        builder.worker_threads(n);
    }
    builder.enable_all().build()
}

fn main() -> std::io::Result<()> {
    build_tokio_runtime()?.block_on(async_main())
}

async fn async_main() -> std::io::Result<()> {
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
    tracing::info!("aero-gateway-rs listening on http://{bind_addr}");
    axum::serve(listener, app).await
}
