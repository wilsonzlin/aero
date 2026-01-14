use aero_storage_server::DEFAULT_MAX_CONCURRENT_BYTES_REQUESTS;
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

    /// Maximum number of bytes allowed to be served for a single chunk object in chunked disk
    /// image delivery (`/v1/images/:image_id/chunked/chunks/...`).
    ///
    /// This is a basic DoS hardening knob to prevent pathological chunk reads.
    ///
    /// Environment variable: `AERO_STORAGE_MAX_CHUNK_BYTES`.
    #[arg(long, env = "AERO_STORAGE_MAX_CHUNK_BYTES")]
    max_chunk_bytes: Option<u64>,

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

    /// Disable the `/metrics` endpoint entirely (it will not be mounted, so requests return `404`).
    ///
    /// Environment variable: `AERO_STORAGE_DISABLE_METRICS`.
    #[arg(long)]
    disable_metrics: bool,

    /// Require `Authorization: Bearer <token>` for the `/metrics` endpoint.
    ///
    /// Environment variable: `AERO_STORAGE_METRICS_AUTH_TOKEN`.
    #[arg(long, env = "AERO_STORAGE_METRICS_AUTH_TOKEN")]
    metrics_auth_token: Option<String>,
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
    pub max_chunk_bytes: Option<u64>,
    pub public_cache_max_age_secs: Option<u64>,
    pub cors_preflight_max_age_secs: Option<u64>,
    pub max_concurrent_bytes_requests: usize,
    pub require_range: bool,
    pub disable_metrics: bool,
    pub metrics_auth_token: Option<String>,
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
        let disable_metrics =
            args.disable_metrics || parse_env_bool("AERO_STORAGE_DISABLE_METRICS");

        let metrics_auth_token = args
            .metrics_auth_token
            .or_else(|| env::var("AERO_STORAGE_METRICS_AUTH_TOKEN").ok());
        let metrics_auth_token = metrics_auth_token.and_then(|v| {
            let v = v.trim().to_string();
            (!v.is_empty()).then_some(v)
        });

        Self {
            listen_addr,
            cors_origins,
            cross_origin_resource_policy,
            images_root,
            require_manifest: args.require_manifest,
            log_level,
            max_range_bytes: args.max_range_bytes,
            max_chunk_bytes: args.max_chunk_bytes,
            public_cache_max_age_secs: args.public_cache_max_age_secs,
            cors_preflight_max_age_secs: args.cors_preflight_max_age_secs,
            max_concurrent_bytes_requests: args.max_concurrent_bytes_requests,
            require_range,
            disable_metrics,
            metrics_auth_token,
        }
    }
}

fn parse_env(var: &str) -> Option<SocketAddr> {
    let value = match env::var(var) {
        Ok(v) => v,
        Err(env::VarError::NotPresent) => return None,
        Err(env::VarError::NotUnicode(_)) => {
            eprintln!("warning: {var} must be valid UTF-8; ignoring");
            return None;
        }
    };

    match value.parse::<SocketAddr>() {
        Ok(addr) => Some(addr),
        Err(_) => {
            eprintln!(
                "warning: invalid {var} value: {value:?} (expected socket address like 127.0.0.1:8080); ignoring"
            );
            None
        }
    }
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
        Err(env::VarError::NotUnicode(_)) => {
            eprintln!("warning: {var} must be valid UTF-8; treating as false");
            return false;
        }
    };

    match value.trim().to_ascii_lowercase().as_str() {
        "" => false,
        "1" | "true" | "t" | "yes" | "y" | "on" => true,
        "0" | "false" | "f" | "no" | "n" | "off" => false,
        other => {
            eprintln!(
                "warning: invalid {var} value: {other:?} (expected boolean); treating as false"
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_env, parse_env_bool};

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn parse_env_missing_is_none() {
        let _guard = ENV_LOCK.lock().unwrap();
        const VAR: &str = "AERO_STORAGE_SERVER_TEST_ADDR_MISSING";
        std::env::remove_var(VAR);
        assert!(parse_env(VAR).is_none());
    }

    #[test]
    fn parse_env_valid_socket_addr_is_some() {
        let _guard = ENV_LOCK.lock().unwrap();
        const VAR: &str = "AERO_STORAGE_SERVER_TEST_ADDR_VALID";
        let prev = std::env::var_os(VAR);

        std::env::set_var(VAR, "127.0.0.1:12345");
        assert_eq!(parse_env(VAR), Some("127.0.0.1:12345".parse().unwrap()));

        match prev {
            Some(v) => std::env::set_var(VAR, v),
            None => std::env::remove_var(VAR),
        }
    }

    #[test]
    fn parse_env_invalid_socket_addr_is_none_and_does_not_panic() {
        let _guard = ENV_LOCK.lock().unwrap();
        const VAR: &str = "AERO_STORAGE_SERVER_TEST_ADDR_INVALID";
        let prev = std::env::var_os(VAR);

        std::env::set_var(VAR, "not-a-socket-addr");
        assert!(parse_env(VAR).is_none());

        match prev {
            Some(v) => std::env::set_var(VAR, v),
            None => std::env::remove_var(VAR),
        }
    }

    #[cfg(unix)]
    #[test]
    fn parse_env_invalid_utf8_is_none_and_does_not_panic() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let _guard = ENV_LOCK.lock().unwrap();
        const VAR: &str = "AERO_STORAGE_SERVER_TEST_ADDR_INVALID_UTF8";
        let prev = std::env::var_os(VAR);

        std::env::set_var(VAR, OsString::from_vec(vec![0xFF, 0xFE, 0xFD]));
        assert!(parse_env(VAR).is_none());

        match prev {
            Some(v) => std::env::set_var(VAR, v),
            None => std::env::remove_var(VAR),
        }
    }

    #[test]
    fn parse_env_bool_missing_is_false() {
        let _guard = ENV_LOCK.lock().unwrap();
        const VAR: &str = "AERO_STORAGE_SERVER_TEST_BOOL_MISSING";
        std::env::remove_var(VAR);
        assert!(!parse_env_bool(VAR));
    }

    #[test]
    fn parse_env_bool_accepts_true_values() {
        let _guard = ENV_LOCK.lock().unwrap();
        const VAR: &str = "AERO_STORAGE_SERVER_TEST_BOOL_TRUE";
        let prev = std::env::var_os(VAR);

        for v in ["1", "true", "t", "yes", "y", "on", " TRUE ", "On"] {
            std::env::set_var(VAR, v);
            assert!(parse_env_bool(VAR), "expected {v:?} to parse as true");
        }

        match prev {
            Some(v) => std::env::set_var(VAR, v),
            None => std::env::remove_var(VAR),
        }
    }

    #[test]
    fn parse_env_bool_accepts_false_values() {
        let _guard = ENV_LOCK.lock().unwrap();
        const VAR: &str = "AERO_STORAGE_SERVER_TEST_BOOL_FALSE";
        let prev = std::env::var_os(VAR);

        for v in ["0", "false", "f", "no", "n", "off", " False ", "OFF"] {
            std::env::set_var(VAR, v);
            assert!(!parse_env_bool(VAR), "expected {v:?} to parse as false");
        }

        match prev {
            Some(v) => std::env::set_var(VAR, v),
            None => std::env::remove_var(VAR),
        }
    }

    #[test]
    fn parse_env_bool_invalid_value_is_false_and_does_not_panic() {
        let _guard = ENV_LOCK.lock().unwrap();
        const VAR: &str = "AERO_STORAGE_SERVER_TEST_BOOL_INVALID";
        let prev = std::env::var_os(VAR);

        std::env::set_var(VAR, "maybe");
        assert!(!parse_env_bool(VAR));

        match prev {
            Some(v) => std::env::set_var(VAR, v),
            None => std::env::remove_var(VAR),
        }
    }

    #[cfg(unix)]
    #[test]
    fn parse_env_bool_invalid_utf8_is_false_and_does_not_panic() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let _guard = ENV_LOCK.lock().unwrap();
        const VAR: &str = "AERO_STORAGE_SERVER_TEST_BOOL_INVALID_UTF8";
        let prev = std::env::var_os(VAR);

        // Invalid UTF-8 bytes should not panic; we treat the value as false.
        std::env::set_var(VAR, OsString::from_vec(vec![0xFF, 0xFE, 0xFD]));
        assert!(!parse_env_bool(VAR));

        match prev {
            Some(v) => std::env::set_var(VAR, v),
            None => std::env::remove_var(VAR),
        }
    }
}
