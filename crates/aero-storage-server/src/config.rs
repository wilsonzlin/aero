use clap::Parser;
use std::{env, net::SocketAddr, path::PathBuf};

#[derive(Debug, Clone, Parser)]
#[command(name = "aero-storage-server", version, about)]
struct Args {
    /// Address the HTTP server listens on.
    ///
    /// Environment variable: `AERO_STORAGE_LISTEN_ADDR`.
    #[arg(long, env = "AERO_STORAGE_LISTEN_ADDR")]
    listen_addr: Option<SocketAddr>,

    /// Origin to allow for CORS.
    ///
    /// When set to a specific origin (not `*`), `aero-storage-server` will also send
    /// `Access-Control-Allow-Credentials: true` so cookie-authenticated cross-origin fetches can
    /// succeed.
    ///
    /// Environment variable: `AERO_STORAGE_CORS_ORIGIN`.
    #[arg(long, env = "AERO_STORAGE_CORS_ORIGIN")]
    cors_origin: Option<String>,

    /// Cross-Origin-Resource-Policy header value for image bytes responses.
    ///
    /// This is a defence-in-depth header for cross-origin isolation (`COEP: require-corp`). Common
    /// values:
    /// - `same-site` (default)
    /// - `cross-origin`
    /// - `same-origin`
    ///
    /// Environment variable: `AERO_STORAGE_CROSS_ORIGIN_RESOURCE_POLICY`.
    #[arg(long, env = "AERO_STORAGE_CROSS_ORIGIN_RESOURCE_POLICY")]
    cross_origin_resource_policy: Option<String>,

    /// Root directory used by the local filesystem store backend.
    ///
    /// Environment variable: `AERO_STORAGE_IMAGE_ROOT`.
    #[arg(long, env = "AERO_STORAGE_IMAGE_ROOT")]
    images_root: Option<PathBuf>,

    /// Log filter (tracing-subscriber EnvFilter syntax).
    ///
    /// Environment variable: `AERO_STORAGE_LOG_LEVEL`.
    #[arg(long, env = "AERO_STORAGE_LOG_LEVEL")]
    log_level: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub listen_addr: SocketAddr,
    pub cors_origin: Option<String>,
    pub cross_origin_resource_policy: String,
    pub images_root: PathBuf,
    pub log_level: String,
}

impl Config {
    pub fn load() -> Self {
        let args = Args::parse();

        let listen_addr = args
            .listen_addr
            .or_else(|| parse_env("AERO_BIND"))
            .or_else(|| parse_env("AERO_STORAGE_SERVER_ADDR"))
            .or_else(|| parse_env("AERO_STORAGE_SERVER_LISTEN_ADDR"))
            .unwrap_or_else(|| "0.0.0.0:8080".parse().expect("default listen addr"));

        let images_root = args
            .images_root
            .or_else(|| env::var("AERO_IMAGE_ROOT").ok().map(PathBuf::from))
            .or_else(|| env::var("AERO_IMAGE_DIR").ok().map(PathBuf::from))
            .or_else(|| {
                env::var("AERO_STORAGE_SERVER_IMAGES_ROOT")
                    .ok()
                    .map(PathBuf::from)
            })
            .unwrap_or_else(|| PathBuf::from("./images"));

        let cors_origin = args
            .cors_origin
            .or_else(|| env::var("AERO_STORAGE_SERVER_CORS_ORIGIN").ok());
        let cors_origin = cors_origin.and_then(|v| {
            let v = v.trim().to_string();
            (!v.is_empty()).then_some(v)
        });

        let cross_origin_resource_policy = args
            .cross_origin_resource_policy
            .or_else(|| env::var("AERO_STORAGE_CORP").ok())
            .or_else(|| env::var("AERO_STORAGE_SERVER_CROSS_ORIGIN_RESOURCE_POLICY").ok())
            .unwrap_or_else(|| "same-site".to_string());
        let cross_origin_resource_policy = cross_origin_resource_policy.trim().to_string();
        let cross_origin_resource_policy = if cross_origin_resource_policy.is_empty() {
            "same-site".to_string()
        } else {
            cross_origin_resource_policy
        };

        let log_level = args
            .log_level
            .or_else(|| env::var("AERO_STORAGE_SERVER_LOG_LEVEL").ok())
            .unwrap_or_else(|| "info".to_string());

        Self {
            listen_addr,
            cors_origin,
            cross_origin_resource_policy,
            images_root,
            log_level,
        }
    }
}

fn parse_env(var: &str) -> Option<SocketAddr> {
    env::var(var).ok().and_then(|value| value.parse().ok())
}
