#![forbid(unsafe_code)]

use std::net::SocketAddr;

use disk_gateway::Config;
use tracing_subscriber::EnvFilter;

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

fn main() {
    let rt = match build_tokio_runtime() {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("disk-gateway: failed to initialize Tokio runtime: {err}");
            std::process::exit(1);
        }
    };
    rt.block_on(async_main())
}

async fn async_main() {
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
