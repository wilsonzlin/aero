#![forbid(unsafe_code)]

use std::net::SocketAddr;

use disk_gateway::Config;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cfg = match Config::from_env() {
        Ok(cfg) => cfg,
        Err(err) => {
            eprintln!("disk-gateway: {err}");
            std::process::exit(2);
        }
    };

    let addr: SocketAddr = cfg
        .bind
        .parse()
        .unwrap_or_else(|_| panic!("invalid DISK_GATEWAY_BIND: {}", cfg.bind));

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .unwrap_or_else(|err| panic!("failed to bind {addr}: {err}"));

    tracing::info!(
        bind = %addr,
        public_dir = %cfg.public_dir.display(),
        private_dir = %cfg.private_dir.display(),
        "disk-gateway listening"
    );

    let app = disk_gateway::app(cfg);
    if let Err(err) = axum::serve(listener, app).await {
        eprintln!("disk-gateway server error: {err}");
        std::process::exit(1);
    }
}

