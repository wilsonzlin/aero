use clap::Parser;
use aero_storage_server::DEFAULT_MAX_CONCURRENT_BYTES_REQUESTS;
use std::{env, net::SocketAddr, path::PathBuf};

#[derive(Debug, Clone, Parser)]
#[command(name = "aero-storage-server", version, about)]
struct Args {
    /// Address the HTTP server listens on.
    ///
    /// Environment variable: `AERO_STORAGE_LISTEN_ADDR`.
    #[arg(long, env = "AERO_STORAGE_LISTEN_ADDR")]
    listen_addr: Option<SocketAddr>,

    /// Origin(s) to allow for CORS.
    ///
    /// This flag can be repeated or provided as a comma-separated list.
    ///
    /// - When set to `*`, `aero-storage-server` will respond with `Access-Control-Allow-Origin: *`
    ///   and will NOT send `Access-Control-Allow-Credentials`.
    /// - When set to a list of origins, `aero-storage-server` will echo back the request `Origin`
    ///   if and only if it appears in the allowlist.
    ///
    /// Environment variable: `AERO_STORAGE_CORS_ORIGIN`.
    #[arg(long, env = "AERO_STORAGE_CORS_ORIGIN", value_delimiter = ',', num_args = 1..)]
    cors_origin: Vec<String>,

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

    /// Require `manifest.json` to exist under `--images-root`.
    ///
    /// When set, the server will refuse to fall back to directory listing when `manifest.json` is
    /// missing. This is recommended in production to avoid accidentally exposing files placed in
    /// the images directory.
    ///
    /// Environment variable: `AERO_STORAGE_REQUIRE_MANIFEST`.
    #[arg(long, env = "AERO_STORAGE_REQUIRE_MANIFEST")]
    require_manifest: bool,

    /// Log filter (tracing-subscriber EnvFilter syntax).
    ///
    /// Environment variable: `AERO_STORAGE_LOG_LEVEL`.
    #[arg(long, env = "AERO_STORAGE_LOG_LEVEL")]
    log_level: Option<String>,
    /// Maximum number of bytes allowed to be served for a single `Range` request.
    ///
    /// Environment variable: `AERO_STORAGE_MAX_RANGE_BYTES`.
    #[arg(long, env = "AERO_STORAGE_MAX_RANGE_BYTES")]
    max_range_bytes: Option<u64>,

    /// Cache max-age (in seconds) used for publicly cacheable disk image bytes responses.
    ///
    /// Environment variable: `AERO_STORAGE_PUBLIC_CACHE_MAX_AGE_SECS`.
    #[arg(long, env = "AERO_STORAGE_PUBLIC_CACHE_MAX_AGE_SECS")]
    public_cache_max_age_secs: Option<u64>,

    /// Preflight cache duration (in seconds) used for `Access-Control-Max-Age`.
    ///
    /// Environment variable: `AERO_STORAGE_CORS_PREFLIGHT_MAX_AGE_SECS`.
    #[arg(long, env = "AERO_STORAGE_CORS_PREFLIGHT_MAX_AGE_SECS")]
    cors_preflight_max_age_secs: Option<u64>,

    /// Maximum concurrent requests to the image bytes endpoints (`/v1/images/:image_id` and
    /// `/v1/images/:image_id/data`).
    ///
    /// Set to 0 to disable limiting (unlimited).
    ///
    /// Environment variable: `AERO_STORAGE_MAX_CONCURRENT_BYTES_REQUESTS`.
    #[arg(
        long,
        env = "AERO_STORAGE_MAX_CONCURRENT_BYTES_REQUESTS",
        default_value_t = DEFAULT_MAX_CONCURRENT_BYTES_REQUESTS
    )]
    max_concurrent_bytes_requests: usize,
    /// Require `Range` requests for image bytes endpoints (`GET /v1/images/:id`).
    ///
    /// When enabled, `GET` requests without a `Range` header will be rejected with
    /// `416 Range Not Satisfiable` (the server will not stream the full image body).
    ///
    /// Environment variable: `AERO_STORAGE_REQUIRE_RANGE`.
    #[arg(long)]
    require_range: bool,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub listen_addr: SocketAddr,
    pub cors_origins: Option<Vec<String>>,
    pub cross_origin_resource_policy: String,
    pub images_root: PathBuf,
    pub require_manifest: bool,
    pub log_level: String,
    pub max_range_bytes: Option<u64>,
    pub public_cache_max_age_secs: Option<u64>,
    pub cors_preflight_max_age_secs: Option<u64>,
    pub max_concurrent_bytes_requests: usize,
    pub require_range: bool,
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

        let cors_origin = args.cors_origin;
        let cors_origins = if !cors_origin.is_empty() {
            Some(parse_origin_list(&cors_origin.join(",")))
        } else if let Ok(v) = env::var("AERO_STORAGE_SERVER_CORS_ORIGIN") {
            let v = v.trim();
            (!v.is_empty()).then(|| parse_origin_list(v))
        } else {
            None
        };
        let cors_origins = cors_origins.and_then(|v| (!v.is_empty()).then_some(v));

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

        let require_range = args.require_range || parse_env_bool("AERO_STORAGE_REQUIRE_RANGE");

        Self {
            listen_addr,
            cors_origins,
            cross_origin_resource_policy,
            images_root,
            require_manifest: args.require_manifest,
            log_level,
            max_range_bytes: args.max_range_bytes,
            public_cache_max_age_secs: args.public_cache_max_age_secs,
            cors_preflight_max_age_secs: args.cors_preflight_max_age_secs,
            require_range,
            max_concurrent_bytes_requests: args.max_concurrent_bytes_requests,
        }
    }
}

fn parse_env(var: &str) -> Option<SocketAddr> {
    env::var(var).ok().and_then(|value| value.parse().ok())
}

fn parse_origin_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .collect()
}

fn parse_env_bool(var: &str) -> bool {
    let value = match env::var(var) {
        Ok(v) => v,
        Err(env::VarError::NotPresent) => return false,
        Err(env::VarError::NotUnicode(_)) => panic!("{var} must be valid UTF-8"),
    };

    match value.trim().to_ascii_lowercase().as_str() {
        "" => false,
        "1" | "true" | "yes" | "y" | "on" => true,
        "0" | "false" | "no" | "n" | "off" => false,
        other => panic!("{var} must be a boolean (got {other:?})"),
    }
}
