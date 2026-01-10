use aero_net_proxy_server::{start_proxy_server, ProxyServerOptions};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let handle = start_proxy_server(ProxyServerOptions::default()).await?;
    println!("proxy server listening on {}", handle.local_addr());
    futures_util::future::pending::<()>().await;
    Ok(())
}
