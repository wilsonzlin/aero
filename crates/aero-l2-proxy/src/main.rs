#![forbid(unsafe_code)]

#[cfg(not(target_arch = "wasm32"))]
use std::{io::Write, net::SocketAddr, str::FromStr};

#[cfg(not(target_arch = "wasm32"))]
use aero_l2_proxy::{start_server, ProxyConfig};

#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
struct CliArgs {
    bind: Option<SocketAddr>,
    ready_stdout: bool,
}

#[cfg(not(target_arch = "wasm32"))]
fn parse_args() -> Result<CliArgs, String> {
    let mut out = CliArgs::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--ready-stdout" {
            out.ready_stdout = true;
            continue;
        }

        if arg == "--bind" {
            let value = args
                .next()
                .ok_or_else(|| "--bind requires a value like 127.0.0.1:0".to_string())?;
            out.bind = Some(
                SocketAddr::from_str(&value)
                    .map_err(|_| format!("invalid --bind value {value:?}"))?,
            );
            continue;
        }

        if let Some(value) = arg.strip_prefix("--bind=") {
            out.bind = Some(
                SocketAddr::from_str(value)
                    .map_err(|_| format!("invalid --bind value {value:?}"))?,
            );
            continue;
        }

        if arg == "--help" || arg == "-h" {
            println!(
                "Usage: aero-l2-proxy [--bind <ip:port>] [--ready-stdout]\n\
                 \n\
                 Options:\n\
                 \t--bind <ip:port>\tOverride the bind address (env: AERO_L2_PROXY_LISTEN_ADDR)\n\
                 \t--ready-stdout\t\tPrint AERO_L2_PROXY_READY <ws-url> once listening"
            );
            std::process::exit(0);
        }

        return Err(format!("unknown argument {arg:?}"));
    }

    Ok(out)
}

#[cfg(not(target_arch = "wasm32"))]
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

#[cfg(not(target_arch = "wasm32"))]
fn build_tokio_runtime() -> std::io::Result<tokio::runtime::Runtime> {
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    if let Some(n) = tokio_worker_threads_from_env() {
        builder.worker_threads(n);
    }
    builder.enable_all().build()
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> std::io::Result<()> {
    build_tokio_runtime()?.block_on(async_main())
}

#[cfg(not(target_arch = "wasm32"))]
async fn async_main() -> std::io::Result<()> {
    let cli = match parse_args() {
        Ok(cli) => cli,
        Err(err) => {
            eprintln!("error: {err}");
            eprintln!("Run with --help for usage.");
            std::process::exit(2);
        }
    };

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let mut config = match ProxyConfig::from_env() {
        Ok(config) => config,
        Err(err) => {
            tracing::error!("invalid config: {err:#}");
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, err));
        }
    };

    if let Some(bind) = cli.bind {
        config.bind_addr = bind;
    }

    let handle = start_server(config).await?;
    let local_addr = handle.local_addr();

    if cli.ready_stdout {
        let host = match local_addr.ip() {
            std::net::IpAddr::V4(ip) => ip.to_string(),
            std::net::IpAddr::V6(ip) => format!("[{ip}]"),
        };
        println!("AERO_L2_PROXY_READY ws://{host}:{}/l2", local_addr.port());
        let _ = std::io::stdout().flush();
    }

    tracing::info!("aero-l2-proxy listening on http://{local_addr}");

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

#[cfg(target_arch = "wasm32")]
fn main() {}
