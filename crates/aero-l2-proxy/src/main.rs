#![forbid(unsafe_code)]

use aero_l2_proxy::{start_server, ProxyConfig};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = match ProxyConfig::from_env() {
        Ok(config) => config,
        Err(err) => {
            tracing::error!("invalid config: {err:#}");
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, err));
        }
    };

    let handle = start_server(config).await?;
    tracing::info!("aero-l2-proxy listening on http://{}", handle.local_addr());

    // Best-effort graceful shutdown on Ctrl+C / SIGTERM.
    let ctrl_c = tokio::signal::ctrl_c();

    #[cfg(unix)]
    let sigterm = async {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut sigterm) => {
                sigterm.recv().await;
            }
            Err(err) => {
                tracing::warn!("failed to install SIGTERM handler: {err}");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let sigterm = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = sigterm => {},
    }

    tracing::info!("shutdown signal received");
    handle.mark_shutting_down();
    handle.shutdown().await;
    Ok(())
}
