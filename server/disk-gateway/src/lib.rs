#![forbid(unsafe_code)]

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_stream::try_stream;
use axum::body::Body;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::header::{
    ACCEPT_RANGES, ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS,
    ACCESS_CONTROL_ALLOW_ORIGIN, ACCESS_CONTROL_EXPOSE_HEADERS, ACCESS_CONTROL_MAX_AGE,
    AUTHORIZATION, CACHE_CONTROL, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE,
    ETAG, IF_NONE_MATCH, IF_MODIFIED_SINCE, IF_RANGE, LAST_MODIFIED, ORIGIN, RANGE, VARY,
};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, options, post};
use axum::Router;
use bytes::Bytes;
use futures_util::StreamExt;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use rand::{distributions::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
use tokio_util::io::ReaderStream;

use aero_http_range::{
    parse_range_header, resolve_ranges, RangeParseError, RangeResolveError, ResolvedByteRange,
};

fn append_vary(headers: &mut HeaderMap, tokens: &[&str]) {
    // Keep behavior consistent with `aero-storage-server`'s CORS implementation: append `Vary`
    // tokens without clobbering any preexisting values, and treat `Vary: *` as a sentinel that
    // should remain intact.
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut has_star = false;

    for value in headers.get_all(VARY).iter() {
        let Ok(value) = value.to_str() else {
            continue;
        };
        for raw in value.split(',') {
            let token = raw.trim();
            if token.is_empty() {
                continue;
            }
            if token == "*" {
                has_star = true;
                break;
            }
            let key = token.to_ascii_lowercase();
            if seen.insert(key) {
                out.push(token.to_string());
            }
        }
        if has_star {
            break;
        }
    }

    if has_star {
        headers.remove(VARY);
        headers.insert(VARY, HeaderValue::from_static("*"));
        return;
    }

    for token in tokens {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        if token == "*" {
            headers.remove(VARY);
            headers.insert(VARY, HeaderValue::from_static("*"));
            return;
        }
        let key = token.to_ascii_lowercase();
        if seen.insert(key) {
            out.push(token.to_string());
        }
    }

    if out.is_empty() {
        return;
    }

    let normalized = out.join(", ");
    if let Ok(value) = HeaderValue::from_str(&normalized) {
        headers.remove(VARY);
        headers.insert(VARY, value);
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub bind: SocketAddr,
    pub public_dir: PathBuf,
    pub private_dir: PathBuf,
    pub token_secret: String,
    pub cors_allowed_origins: AllowedOrigins,
    pub corp_policy: CorpPolicy,
    pub lease_ttl: Duration,
    pub max_ranges: usize,
    pub max_total_bytes: u64,
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let bind = match std::env::var("DISK_GATEWAY_BIND") {
            Ok(v) => v,
            Err(std::env::VarError::NotPresent) => "127.0.0.1:3000".to_owned(),
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err(ConfigError::InvalidEnv("DISK_GATEWAY_BIND"));
            }
        };
        let bind: SocketAddr = bind
            .parse()
            .map_err(|_| ConfigError::InvalidEnv("DISK_GATEWAY_BIND"))?;

        let public_dir = std::env::var_os("DISK_GATEWAY_PUBLIC_DIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("./public-images"));
        let private_dir = std::env::var_os("DISK_GATEWAY_PRIVATE_DIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("./private-images"));

        let token_secret = match std::env::var("DISK_GATEWAY_TOKEN_SECRET") {
            Ok(v) if !v.trim().is_empty() => v,
            Ok(_) => return Err(ConfigError::InvalidEnv("DISK_GATEWAY_TOKEN_SECRET")),
            Err(std::env::VarError::NotPresent) => {
                return Err(ConfigError::MissingEnv("DISK_GATEWAY_TOKEN_SECRET"));
            }
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err(ConfigError::InvalidEnv("DISK_GATEWAY_TOKEN_SECRET"));
            }
        };
        let cors_allowed_origins = AllowedOrigins::from_env()?;
        let corp_policy = CorpPolicy::from_env()?;
        let lease_ttl = std::env::var("DISK_GATEWAY_LEASE_TTL_SECONDS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or_else(|| Duration::from_secs(15 * 60));
        let max_ranges = std::env::var("DISK_GATEWAY_MAX_RANGES")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(16);
        let max_total_bytes = std::env::var("DISK_GATEWAY_MAX_TOTAL_BYTES")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(512 * 1024 * 1024);

        Ok(Self {
            bind,
            public_dir,
            private_dir,
            token_secret,
            cors_allowed_origins,
            corp_policy,
            lease_ttl,
            max_ranges,
            max_total_bytes,
        })
    }
}

#[derive(Debug)]
pub enum ConfigError {
    MissingEnv(&'static str),
    InvalidEnv(&'static str),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingEnv(var) => write!(f, "missing required env var {var}"),
            Self::InvalidEnv(var) => write!(f, "invalid value for env var {var}"),
        }
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod config_env_tests {
    use super::{Config, ConfigError};

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn set_var_scoped(key: &str, value: &str) -> Option<std::ffi::OsString> {
        let prev = std::env::var_os(key);
        std::env::set_var(key, value);
        prev
    }

    fn restore_var(key: &str, prev: Option<std::ffi::OsString>) {
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    #[test]
    fn from_env_uses_default_bind_when_missing() {
        let _guard = ENV_LOCK.lock().unwrap();

        let prev_bind = std::env::var_os("DISK_GATEWAY_BIND");
        let prev_public = std::env::var_os("DISK_GATEWAY_PUBLIC_DIR");
        let prev_private = std::env::var_os("DISK_GATEWAY_PRIVATE_DIR");
        let prev_cors = std::env::var_os("DISK_GATEWAY_CORS_ALLOWED_ORIGINS");
        let prev_corp = std::env::var_os("DISK_GATEWAY_CORP");
        let prev_secret = set_var_scoped("DISK_GATEWAY_TOKEN_SECRET", "secret");
        std::env::remove_var("DISK_GATEWAY_BIND");
        std::env::remove_var("DISK_GATEWAY_PUBLIC_DIR");
        std::env::remove_var("DISK_GATEWAY_PRIVATE_DIR");
        std::env::remove_var("DISK_GATEWAY_CORS_ALLOWED_ORIGINS");
        std::env::remove_var("DISK_GATEWAY_CORP");

        let cfg = Config::from_env().expect("Config::from_env should succeed with defaults");
        assert_eq!(cfg.bind, "127.0.0.1:3000".parse().unwrap());

        restore_var("DISK_GATEWAY_BIND", prev_bind);
        restore_var("DISK_GATEWAY_PUBLIC_DIR", prev_public);
        restore_var("DISK_GATEWAY_PRIVATE_DIR", prev_private);
        restore_var("DISK_GATEWAY_CORS_ALLOWED_ORIGINS", prev_cors);
        restore_var("DISK_GATEWAY_CORP", prev_corp);
        restore_var("DISK_GATEWAY_TOKEN_SECRET", prev_secret);
    }

    #[test]
    fn from_env_rejects_invalid_bind_without_panicking() {
        let _guard = ENV_LOCK.lock().unwrap();

        let prev_bind = set_var_scoped("DISK_GATEWAY_BIND", "not-a-socket-addr");
        let prev_cors = std::env::var_os("DISK_GATEWAY_CORS_ALLOWED_ORIGINS");
        let prev_corp = std::env::var_os("DISK_GATEWAY_CORP");
        let prev_secret = set_var_scoped("DISK_GATEWAY_TOKEN_SECRET", "secret");
        std::env::remove_var("DISK_GATEWAY_CORS_ALLOWED_ORIGINS");
        std::env::remove_var("DISK_GATEWAY_CORP");

        let err = Config::from_env().expect_err("expected Config::from_env to reject invalid bind");
        match err {
            ConfigError::InvalidEnv(var) => assert_eq!(var, "DISK_GATEWAY_BIND"),
            other => panic!("expected InvalidEnv, got {other:?}"),
        }

        restore_var("DISK_GATEWAY_BIND", prev_bind);
        restore_var("DISK_GATEWAY_CORS_ALLOWED_ORIGINS", prev_cors);
        restore_var("DISK_GATEWAY_CORP", prev_corp);
        restore_var("DISK_GATEWAY_TOKEN_SECRET", prev_secret);
    }

    #[test]
    fn from_env_rejects_empty_token_secret() {
        let _guard = ENV_LOCK.lock().unwrap();

        let prev_bind = std::env::var_os("DISK_GATEWAY_BIND");
        let prev_cors = std::env::var_os("DISK_GATEWAY_CORS_ALLOWED_ORIGINS");
        let prev_corp = std::env::var_os("DISK_GATEWAY_CORP");
        let prev_secret = set_var_scoped("DISK_GATEWAY_TOKEN_SECRET", "   ");
        std::env::remove_var("DISK_GATEWAY_BIND");
        std::env::remove_var("DISK_GATEWAY_CORS_ALLOWED_ORIGINS");
        std::env::remove_var("DISK_GATEWAY_CORP");

        let err = Config::from_env().expect_err("expected Config::from_env to reject empty secret");
        match err {
            ConfigError::InvalidEnv(var) => assert_eq!(var, "DISK_GATEWAY_TOKEN_SECRET"),
            other => panic!("expected InvalidEnv, got {other:?}"),
        }

        restore_var("DISK_GATEWAY_BIND", prev_bind);
        restore_var("DISK_GATEWAY_CORS_ALLOWED_ORIGINS", prev_cors);
        restore_var("DISK_GATEWAY_CORP", prev_corp);
        restore_var("DISK_GATEWAY_TOKEN_SECRET", prev_secret);
    }

    #[cfg(unix)]
    #[test]
    fn from_env_rejects_non_utf8_bind() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let _guard = ENV_LOCK.lock().unwrap();

        let prev_bind =
            set_var_scoped_os("DISK_GATEWAY_BIND", OsString::from_vec(vec![0xFF, 0xFE, 0xFD]));
        let prev_cors = std::env::var_os("DISK_GATEWAY_CORS_ALLOWED_ORIGINS");
        let prev_corp = std::env::var_os("DISK_GATEWAY_CORP");
        let prev_secret = set_var_scoped("DISK_GATEWAY_TOKEN_SECRET", "secret");
        std::env::remove_var("DISK_GATEWAY_CORS_ALLOWED_ORIGINS");
        std::env::remove_var("DISK_GATEWAY_CORP");

        let err = Config::from_env().expect_err("expected Config::from_env to reject non-utf8 bind");
        match err {
            ConfigError::InvalidEnv(var) => assert_eq!(var, "DISK_GATEWAY_BIND"),
            other => panic!("expected InvalidEnv, got {other:?}"),
        }

        restore_var("DISK_GATEWAY_BIND", prev_bind);
        restore_var("DISK_GATEWAY_CORS_ALLOWED_ORIGINS", prev_cors);
        restore_var("DISK_GATEWAY_CORP", prev_corp);
        restore_var("DISK_GATEWAY_TOKEN_SECRET", prev_secret);
    }

    #[cfg(unix)]
    #[test]
    fn from_env_accepts_non_utf8_public_dir() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::ffi::OsStringExt;

        let _guard = ENV_LOCK.lock().unwrap();

        let prev_public = set_var_scoped_os(
            "DISK_GATEWAY_PUBLIC_DIR",
            OsString::from_vec(vec![b'p', b'u', b'b', 0xFF]),
        );
        let prev_private = std::env::var_os("DISK_GATEWAY_PRIVATE_DIR");
        let prev_bind = std::env::var_os("DISK_GATEWAY_BIND");
        let prev_cors = std::env::var_os("DISK_GATEWAY_CORS_ALLOWED_ORIGINS");
        let prev_corp = std::env::var_os("DISK_GATEWAY_CORP");
        let prev_secret = set_var_scoped("DISK_GATEWAY_TOKEN_SECRET", "secret");
        std::env::remove_var("DISK_GATEWAY_PRIVATE_DIR");
        std::env::remove_var("DISK_GATEWAY_BIND");
        std::env::remove_var("DISK_GATEWAY_CORS_ALLOWED_ORIGINS");
        std::env::remove_var("DISK_GATEWAY_CORP");

        let cfg = Config::from_env().expect("expected Config::from_env to accept non-utf8 dirs");
        assert_eq!(
            cfg.public_dir.as_os_str().as_bytes(),
            [b'p', b'u', b'b', 0xFF]
        );

        restore_var("DISK_GATEWAY_PUBLIC_DIR", prev_public);
        restore_var("DISK_GATEWAY_PRIVATE_DIR", prev_private);
        restore_var("DISK_GATEWAY_BIND", prev_bind);
        restore_var("DISK_GATEWAY_CORS_ALLOWED_ORIGINS", prev_cors);
        restore_var("DISK_GATEWAY_CORP", prev_corp);
        restore_var("DISK_GATEWAY_TOKEN_SECRET", prev_secret);
    }

    #[cfg(unix)]
    #[test]
    fn from_env_rejects_non_utf8_cors_allowed_origins() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let _guard = ENV_LOCK.lock().unwrap();

        let prev_bind = std::env::var_os("DISK_GATEWAY_BIND");
        let prev_public = std::env::var_os("DISK_GATEWAY_PUBLIC_DIR");
        let prev_private = std::env::var_os("DISK_GATEWAY_PRIVATE_DIR");
        let prev_cors = set_var_scoped_os(
            "DISK_GATEWAY_CORS_ALLOWED_ORIGINS",
            OsString::from_vec(vec![0xFF, 0xFE, 0xFD]),
        );
        let prev_corp = std::env::var_os("DISK_GATEWAY_CORP");
        let prev_secret = set_var_scoped("DISK_GATEWAY_TOKEN_SECRET", "secret");
        std::env::remove_var("DISK_GATEWAY_BIND");
        std::env::remove_var("DISK_GATEWAY_PUBLIC_DIR");
        std::env::remove_var("DISK_GATEWAY_PRIVATE_DIR");
        std::env::remove_var("DISK_GATEWAY_CORP");

        let err = Config::from_env()
            .expect_err("expected Config::from_env to reject non-utf8 cors origins");
        match err {
            ConfigError::InvalidEnv(var) => assert_eq!(var, "DISK_GATEWAY_CORS_ALLOWED_ORIGINS"),
            other => panic!("expected InvalidEnv, got {other:?}"),
        }

        restore_var("DISK_GATEWAY_BIND", prev_bind);
        restore_var("DISK_GATEWAY_PUBLIC_DIR", prev_public);
        restore_var("DISK_GATEWAY_PRIVATE_DIR", prev_private);
        restore_var("DISK_GATEWAY_CORS_ALLOWED_ORIGINS", prev_cors);
        restore_var("DISK_GATEWAY_CORP", prev_corp);
        restore_var("DISK_GATEWAY_TOKEN_SECRET", prev_secret);
    }

    #[cfg(unix)]
    fn set_var_scoped_os(key: &str, value: std::ffi::OsString) -> Option<std::ffi::OsString> {
        let prev = std::env::var_os(key);
        std::env::set_var(key, value);
        prev
    }
}

#[derive(Clone, Debug)]
pub enum AllowedOrigins {
    Any,
    List(HashSet<String>),
}

impl AllowedOrigins {
    fn from_env() -> Result<Self, ConfigError> {
        let raw = match std::env::var("DISK_GATEWAY_CORS_ALLOWED_ORIGINS") {
            Ok(v) => v,
            Err(std::env::VarError::NotPresent) => "*".to_owned(),
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err(ConfigError::InvalidEnv("DISK_GATEWAY_CORS_ALLOWED_ORIGINS"));
            }
        };
        let raw = raw.trim();
        if raw == "*" || raw.is_empty() {
            return Ok(Self::Any);
        }

        let list: HashSet<String> = raw
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
            .collect();

        if list.is_empty() {
            return Err(ConfigError::InvalidEnv("DISK_GATEWAY_CORS_ALLOWED_ORIGINS"));
        }

        Ok(Self::List(list))
    }

    fn resolve(&self, request_origin: Option<&HeaderValue>) -> Option<HeaderValue> {
        match self {
            Self::Any => Some(HeaderValue::from_static("*")),
            Self::List(list) => {
                let origin = request_origin?.to_str().ok()?;
                if list.contains(origin) {
                    Some(HeaderValue::from_str(origin).ok()?)
                } else {
                    None
                }
            }
        }
    }

    fn should_vary_origin(&self) -> bool {
        matches!(self, Self::List(_))
    }
}

#[derive(Clone, Copy, Debug)]
pub enum CorpPolicy {
    SameSite,
    CrossOrigin,
}

impl CorpPolicy {
    fn from_env() -> Result<Self, ConfigError> {
        let raw = match std::env::var("DISK_GATEWAY_CORP") {
            Ok(v) => v,
            Err(std::env::VarError::NotPresent) => "same-site".to_owned(),
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err(ConfigError::InvalidEnv("DISK_GATEWAY_CORP"));
            }
        };
        match raw.trim() {
            "same-site" => Ok(Self::SameSite),
            "cross-origin" => Ok(Self::CrossOrigin),
            _ => Err(ConfigError::InvalidEnv("DISK_GATEWAY_CORP")),
        }
    }

    fn as_header_value(self) -> HeaderValue {
        match self {
            Self::SameSite => HeaderValue::from_static("same-site"),
            Self::CrossOrigin => HeaderValue::from_static("cross-origin"),
        }
    }
}

#[derive(Clone)]
struct AppState {
    cfg: Config,
}

pub fn app(cfg: Config) -> Router {
    let state = Arc::new(AppState { cfg });

    let api_router = Router::new()
        .route("/images/:id/lease", post(lease_post).options(api_options))
        .route("/*path", options(api_options))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            api_headers_middleware,
        ));

    let disk_router = Router::new()
        .route("/:id", get(disk_get).head(disk_head).options(disk_options))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            disk_headers_middleware,
        ));

    Router::new()
        .nest("/api", api_router)
        .nest("/disk", disk_router)
        .with_state(state)
}

async fn api_headers_middleware(
    State(state): State<Arc<AppState>>,
    req: Request<Body>,
    next: axum::middleware::Next,
) -> Response {
    let origin = req.headers().get(ORIGIN).cloned();
    let mut resp = next.run(req).await;
    apply_cors_headers(&state.cfg, origin.as_ref(), &mut resp, false, "");
    resp
}

async fn disk_headers_middleware(
    State(state): State<Arc<AppState>>,
    req: Request<Body>,
    next: axum::middleware::Next,
) -> Response {
    let has_auth = req.headers().contains_key(AUTHORIZATION)
        || req
            .uri()
            .query()
            .map(|q| q.split('&').any(|kv| kv.starts_with("token=")))
            .unwrap_or(false);
    let origin = req.headers().get(ORIGIN).cloned();
    let mut resp = next.run(req).await;
    apply_cors_headers(&state.cfg, origin.as_ref(), &mut resp, false, "");
    resp.headers_mut().insert(
        CACHE_CONTROL,
        if has_auth {
            HeaderValue::from_static("private, no-store, no-transform")
        } else {
            HeaderValue::from_static("no-transform")
        },
    );
    // Disk bytes are served as raw, deterministic offsets; never allow compression transforms.
    if !resp.headers().contains_key(CONTENT_ENCODING) {
        resp.headers_mut()
            .insert(CONTENT_ENCODING, HeaderValue::from_static("identity"));
    }
    resp.headers_mut().insert(
        HeaderName::from_static("cross-origin-resource-policy"),
        state.cfg.corp_policy.as_header_value(),
    );
    resp.headers_mut().insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    resp
}

#[derive(Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

async fn api_options(State(state): State<Arc<AppState>>, req: Request<Body>) -> Response {
    preflight_response(&state.cfg, req.headers(), "GET, HEAD, POST, OPTIONS")
}

async fn disk_options(State(state): State<Arc<AppState>>, req: Request<Body>) -> Response {
    preflight_response(&state.cfg, req.headers(), "GET, HEAD, OPTIONS")
}

fn preflight_response(cfg: &Config, headers: &HeaderMap, allow_methods: &'static str) -> Response {
    let mut resp = StatusCode::NO_CONTENT.into_response();
    let origin = headers.get(ORIGIN);
    apply_cors_headers(cfg, origin, &mut resp, true, allow_methods);
    resp
}

fn apply_cors_headers(
    cfg: &Config,
    request_origin: Option<&HeaderValue>,
    resp: &mut Response,
    is_preflight: bool,
    allow_methods: &'static str,
) {
    if let Some(allow_origin) = cfg.cors_allowed_origins.resolve(request_origin) {
        resp.headers_mut()
            .insert(ACCESS_CONTROL_ALLOW_ORIGIN, allow_origin);

        if cfg.cors_allowed_origins.should_vary_origin() {
            append_vary(resp.headers_mut(), &["Origin"]);
        }

        resp.headers_mut().insert(
            ACCESS_CONTROL_EXPOSE_HEADERS,
            HeaderValue::from_static(
                "Accept-Ranges, Content-Range, Content-Length, Content-Encoding, ETag, Last-Modified",
            ),
        );
    }

    if is_preflight {
        // Even when `Access-Control-Allow-Origin: *`, varying on the preflight request headers is a
        // safe default for caches and avoids surprising behavior if deployments later move to an
        // allowlist.
        append_vary(
            resp.headers_mut(),
            &[
                "Origin",
                "Access-Control-Request-Method",
                "Access-Control-Request-Headers",
            ],
        );
        resp.headers_mut().insert(
            ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static(allow_methods),
        );
        resp.headers_mut().insert(
            ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static(
                "Range, If-Range, If-None-Match, If-Modified-Since, Authorization, Content-Type",
            ),
        );
        resp.headers_mut()
            .insert(ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("86400"));
    }
}

fn is_safe_path_segment(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'_' | b'-'))
}

fn public_image_path(cfg: &Config, image_id: &str) -> PathBuf {
    cfg.public_dir.join(format!("{image_id}.img"))
}

fn private_image_path(cfg: &Config, user_id: &str, image_id: &str) -> PathBuf {
    cfg.private_dir
        .join(user_id)
        .join(format!("{image_id}.img"))
}

#[derive(Debug, Serialize, Deserialize)]
struct LeaseClaims {
    img: String,
    sub: String,
    scope: String,
    exp: usize,
}

fn scope_allows_disk_read(scope: &str) -> bool {
    scope.split_whitespace().any(|s| s == "disk:read")
}

const HMAC_SHA256_SIG_B64URL_LEN: usize = 43; // 32 bytes -> 43 chars, base64url no-pad
const MAX_JWT_HEADER_B64URL_LEN: usize = 4 * 1024;
const MAX_JWT_PAYLOAD_B64URL_LEN: usize = 16 * 1024;
const MAX_JWT_LEN: usize = MAX_JWT_HEADER_B64URL_LEN
    + 1
    + MAX_JWT_PAYLOAD_B64URL_LEN
    + 1
    + HMAC_SHA256_SIG_B64URL_LEN;

fn b64url_value(b: u8) -> Option<u8> {
    match b {
        b'A'..=b'Z' => Some(b - b'A'),
        b'a'..=b'z' => Some(b - b'a' + 26),
        b'0'..=b'9' => Some(b - b'0' + 52),
        b'-' => Some(62),
        b'_' => Some(63),
        _ => None,
    }
}

fn is_base64url_no_pad(raw: &str, max_len: usize) -> bool {
    if raw.is_empty() || raw.len() > max_len {
        return false;
    }
    let rem = raw.len() % 4;
    if rem == 1 {
        return false;
    }
    raw.as_bytes().iter().all(|&b| b64url_value(b).is_some())
}

fn is_canonical_base64url_no_pad(raw: &str, max_len: usize) -> bool {
    if !is_base64url_no_pad(raw, max_len) {
        return false;
    }
    let rem = raw.len() % 4;
    if rem == 0 {
        return true;
    }

    let Some(&last) = raw.as_bytes().last() else {
        return false;
    };
    let Some(v) = b64url_value(last) else {
        return false;
    };

    // Canonical base64 requires unused bits be zero:
    // - len % 4 == 2 encodes 1 byte => last char has 4 unused low bits
    // - len % 4 == 3 encodes 2 bytes => last char has 2 unused low bits
    if rem == 2 {
        (v & 0x0f) == 0
    } else {
        (v & 0x03) == 0
    }
}

fn split_jwt_parts(token: &str) -> Option<(&str, &str, &str)> {
    if token.is_empty() || token.len() > MAX_JWT_LEN {
        return None;
    }

    let dot1 = token.find('.')?;
    let dot2 = token[dot1 + 1..].find('.').map(|i| dot1 + 1 + i)?;
    if token[dot2 + 1..].contains('.') {
        return None;
    }
    let header = &token[..dot1];
    let payload = &token[dot1 + 1..dot2];
    let sig = &token[dot2 + 1..];

    if !is_canonical_base64url_no_pad(header, MAX_JWT_HEADER_B64URL_LEN) {
        return None;
    }
    if !is_canonical_base64url_no_pad(payload, MAX_JWT_PAYLOAD_B64URL_LEN) {
        return None;
    }
    if sig.len() != HMAC_SHA256_SIG_B64URL_LEN
        || !is_canonical_base64url_no_pad(sig, HMAC_SHA256_SIG_B64URL_LEN)
    {
        return None;
    }
    Some((header, payload, sig))
}

fn sign_lease(
    cfg: &Config,
    image_id: &str,
    user_id: &str,
    expires_at: OffsetDateTime,
) -> Result<String, ApiError> {
    let claims = LeaseClaims {
        img: image_id.to_owned(),
        sub: user_id.to_owned(),
        scope: "disk:read".to_owned(),
        exp: expires_at
            .unix_timestamp()
            .try_into()
            .map_err(|_| ApiError::Internal)?,
    };

    jsonwebtoken::encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(cfg.token_secret.as_bytes()),
    )
    .map_err(|_| ApiError::Internal)
}

fn verify_lease(cfg: &Config, token: &str) -> Result<LeaseClaims, ApiError> {
    // Defensive: reject attacker-controlled allocations/work before invoking a JWT library decode.
    // This is a public-facing endpoint, and the `token` parameter is user-controlled.
    if split_jwt_parts(token).is_none() {
        return Err(ApiError::Unauthorized);
    }

    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_exp = true;
    let data = jsonwebtoken::decode::<LeaseClaims>(
        token,
        &DecodingKey::from_secret(cfg.token_secret.as_bytes()),
        &validation,
    )
    .map_err(|_| ApiError::Unauthorized)?;

    if !scope_allows_disk_read(&data.claims.scope) {
        return Err(ApiError::Forbidden);
    }

    Ok(data.claims)
}

#[derive(Debug)]
enum ApiError {
    BadRequest(&'static str),
    NotFound,
    Unauthorized,
    Forbidden,
    Internal,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            Self::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg),
            Self::NotFound => (StatusCode::NOT_FOUND, "not found"),
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            Self::Forbidden => (StatusCode::FORBIDDEN, "forbidden"),
            Self::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
        };

        let body = serde_json::json!({ "error": msg });
        (status, axum::Json(body)).into_response()
    }
}

#[derive(Serialize)]
struct LeaseResponse {
    url: String,
    token: Option<String>,
    #[serde(rename = "expiresAt")]
    expires_at: String,
}

async fn lease_post(
    State(state): State<Arc<AppState>>,
    AxumPath(image_id): AxumPath<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if !is_safe_path_segment(&image_id) {
        return Err(ApiError::BadRequest("invalid image id"));
    }

    let public_path = public_image_path(&state.cfg, &image_id);
    if tokio::fs::try_exists(&public_path)
        .await
        .map_err(|_| ApiError::Internal)?
    {
        let expires_at = OffsetDateTime::now_utc() + time::Duration::days(365);
        let expires_at_str = expires_at
            .format(&Rfc3339)
            .map_err(|_| ApiError::Internal)?;
        let body = LeaseResponse {
            url: format!("/disk/{image_id}"),
            token: None,
            expires_at: expires_at_str,
        };

        return Ok(axum::Json(body).into_response());
    }

    let user_id = headers
        .get("X-Debug-User")
        .and_then(|v| v.to_str().ok())
        .or_else(|| {
            headers
                .get(AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .map(str::trim)
        })
        .ok_or(ApiError::Unauthorized)?;

    if !is_safe_path_segment(user_id) {
        return Err(ApiError::BadRequest("invalid user id"));
    }

    let private_path = private_image_path(&state.cfg, user_id, &image_id);
    if !tokio::fs::try_exists(&private_path)
        .await
        .map_err(|_| ApiError::Internal)?
    {
        return Err(ApiError::NotFound);
    }

    let expires_at = OffsetDateTime::now_utc()
        + time::Duration::try_from(state.cfg.lease_ttl).map_err(|_| ApiError::Internal)?;
    let token = sign_lease(&state.cfg, &image_id, user_id, expires_at)?;
    let expires_at_str = expires_at
        .format(&Rfc3339)
        .map_err(|_| ApiError::Internal)?;
    let body = LeaseResponse {
        url: format!("/disk/{image_id}"),
        token: Some(token),
        expires_at: expires_at_str,
    };

    Ok(axum::Json(body).into_response())
}

async fn disk_head(
    State(state): State<Arc<AppState>>,
    AxumPath(image_id): AxumPath<String>,
    Query(query): Query<TokenQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let path = resolve_disk_path(&state.cfg, &image_id, &headers, query.token.as_deref()).await?;
    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => ApiError::NotFound,
            _ => ApiError::Internal,
        })?;
    let size = metadata.len();
    let last_modified = metadata.modified().ok();
    let last_modified_header = last_modified_header_value(last_modified);

    let etag = compute_etag(&metadata);
    let etag_str = etag.to_str().ok();

    if is_not_modified(&headers, etag_str, last_modified) {
        return Ok(not_modified_response(etag.clone(), last_modified_header));
    }

    let mut range_header = headers.get(RANGE).and_then(|v| v.to_str().ok());
    let if_range = headers.get(IF_RANGE).and_then(|v| v.to_str().ok());
    if let (Some(_range), Some(if_range)) = (range_header, if_range) {
        if !if_range_allows_range(if_range, etag_str, last_modified) {
            // RFC 9110: ignore Range when If-Range doesn't match to avoid mixed-version bytes.
            range_header = None;
        }
    }

    let (status, content_type, content_range, content_length) =
        match resolve_request_ranges(&state.cfg, range_header, size) {
            Ok(None) => (StatusCode::OK, None, None, Some(size)),
            Ok(Some(ranges)) if ranges.len() == 1 => {
                let r = ranges[0];
                (
                    StatusCode::PARTIAL_CONTENT,
                    None,
                    Some(format!("bytes {}-{}/{}", r.start, r.end, size)),
                    Some(r.len()),
                )
            }
            Ok(Some(_ranges)) => {
                let boundary = make_boundary();
                (
                    StatusCode::PARTIAL_CONTENT,
                    Some(format!("multipart/byteranges; boundary={boundary}")),
                    None,
                    None,
                )
            }
            Err(RangeRequestError::NotSatisfiable) => {
                return Ok(range_not_satisfiable_response(size))
            }
            Err(RangeRequestError::TooLarge) => return Ok(payload_too_large_response()),
        };

    let mut builder = Response::builder()
        .status(status)
        .header(ACCEPT_RANGES, "bytes")
        .header(
            CONTENT_TYPE,
            content_type.unwrap_or_else(|| "application/octet-stream".to_owned()),
        )
        .header(ETAG, etag);

    if let Some(last_modified_header) = last_modified_header {
        builder = builder.header(LAST_MODIFIED, last_modified_header);
    }

    if let Some(content_range) = content_range {
        builder = builder.header(CONTENT_RANGE, content_range);
    }
    if let Some(content_length) = content_length {
        builder = builder.header(CONTENT_LENGTH, content_length);
    }

    Ok(builder
        .body(Body::empty())
        .map_err(|_| ApiError::Internal)?
        .into_response())
}

async fn disk_get(
    State(state): State<Arc<AppState>>,
    AxumPath(image_id): AxumPath<String>,
    Query(query): Query<TokenQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let path = resolve_disk_path(&state.cfg, &image_id, &headers, query.token.as_deref()).await?;
    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => ApiError::NotFound,
            _ => ApiError::Internal,
    })?;
    let size = metadata.len();
    let last_modified = metadata.modified().ok();
    let last_modified_header = last_modified_header_value(last_modified);
    let etag = compute_etag(&metadata);

    let etag_str = etag.to_str().ok();

    // Conditional requests: If-None-Match dominates If-Modified-Since (RFC 9110).
    if is_not_modified(&headers, etag_str, last_modified) {
        return Ok(not_modified_response(etag.clone(), last_modified_header));
    }

    let mut range_header = headers.get(RANGE).and_then(|v| v.to_str().ok());
    let if_range = headers.get(IF_RANGE).and_then(|v| v.to_str().ok());
    if let (Some(_range), Some(if_range)) = (range_header, if_range) {
        if !if_range_allows_range(if_range, etag_str, last_modified) {
            // RFC 9110: ignore Range when If-Range doesn't match to avoid mixed-version bytes.
            range_header = None;
        }
    }
    let ranges = match resolve_request_ranges(&state.cfg, range_header, size) {
        Ok(r) => r,
        Err(RangeRequestError::NotSatisfiable) => return Ok(range_not_satisfiable_response(size)),
        Err(RangeRequestError::TooLarge) => return Ok(payload_too_large_response()),
    };

    match ranges {
        None => serve_full_file(&path, size, etag, last_modified_header).await,
        Some(ranges) if ranges.len() == 1 => {
            serve_single_range(&path, size, etag, last_modified_header, ranges[0]).await
        }
        Some(ranges) => serve_multi_range(&path, size, etag, last_modified_header, ranges).await,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RangeRequestError {
    NotSatisfiable,
    TooLarge,
}

fn resolve_request_ranges(
    cfg: &Config,
    header_value: Option<&str>,
    size: u64,
) -> Result<Option<Vec<ResolvedByteRange>>, RangeRequestError> {
    let Some(header_value) = header_value else {
        return Ok(None);
    };

    let specs = match parse_range_header(header_value) {
        Ok(specs) => specs,
        Err(RangeParseError::UnsupportedUnit) => return Ok(None),
        Err(RangeParseError::TooManyRanges { .. }) => return Err(RangeRequestError::TooLarge),
        Err(_) => return Err(RangeRequestError::NotSatisfiable),
    };

    // Multi-range abuse guard: cap the number of ranges we will serve and the total payload size.
    if specs.len() > cfg.max_ranges {
        return Err(RangeRequestError::TooLarge);
    }

    let resolved = match resolve_ranges(&specs, size, false) {
        Ok(r) => r,
        Err(RangeResolveError::Unsatisfiable) => return Err(RangeRequestError::NotSatisfiable),
    };

    if resolved.len() > cfg.max_ranges {
        return Err(RangeRequestError::TooLarge);
    }

    let mut total: u64 = 0;
    for r in &resolved {
        total = total
            .checked_add(r.len())
            .ok_or(RangeRequestError::TooLarge)?;
        if total > cfg.max_total_bytes {
            return Err(RangeRequestError::TooLarge);
        }
    }

    Ok(Some(resolved))
}

fn make_boundary() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect()
}

async fn serve_full_file(
    path: &PathBuf,
    size: u64,
    etag: HeaderValue,
    last_modified: Option<HeaderValue>,
) -> Result<Response, ApiError> {
    let file = File::open(path).await.map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => ApiError::NotFound,
        _ => ApiError::Internal,
    })?;

    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(ACCEPT_RANGES, "bytes")
        .header(CONTENT_TYPE, "application/octet-stream")
        .header(CONTENT_LENGTH, size)
        .header(ETAG, etag);

    if let Some(last_modified) = last_modified {
        builder = builder.header(LAST_MODIFIED, last_modified);
    }

    Ok(builder
        .body(Body::from_stream(ReaderStream::new(file)))
        .map_err(|_| ApiError::Internal)?
        .into_response())
}

async fn serve_single_range(
    path: &PathBuf,
    size: u64,
    etag: HeaderValue,
    last_modified: Option<HeaderValue>,
    range: ResolvedByteRange,
) -> Result<Response, ApiError> {
    let mut file = File::open(path).await.map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => ApiError::NotFound,
        _ => ApiError::Internal,
    })?;

    file.seek(SeekFrom::Start(range.start))
        .await
        .map_err(|_| ApiError::Internal)?;

    let len = range.len();

    let mut builder = Response::builder()
        .status(StatusCode::PARTIAL_CONTENT)
        .header(ACCEPT_RANGES, "bytes")
        .header(CONTENT_TYPE, "application/octet-stream")
        .header(
            CONTENT_RANGE,
            format!("bytes {}-{}/{}", range.start, range.end, size),
        )
        .header(CONTENT_LENGTH, len)
        .header(ETAG, etag);

    if let Some(last_modified) = last_modified {
        builder = builder.header(LAST_MODIFIED, last_modified);
    }

    Ok(builder
        .body(Body::from_stream(ReaderStream::new(file.take(len))))
        .map_err(|_| ApiError::Internal)?
        .into_response())
}

async fn serve_multi_range(
    path: &PathBuf,
    size: u64,
    etag: HeaderValue,
    last_modified: Option<HeaderValue>,
    ranges: Vec<ResolvedByteRange>,
) -> Result<Response, ApiError> {
    let boundary = make_boundary();
    let content_type = format!("multipart/byteranges; boundary={boundary}");

    let path = path.clone();
    let boundary_stream = boundary.clone();
    let stream = try_stream! {
        for range in ranges {
            let header = format!(
                "--{boundary_stream}\r\nContent-Type: application/octet-stream\r\nContent-Range: bytes {start}-{end}/{size}\r\n\r\n",
                start = range.start,
                end = range.end,
                size = size,
            );
            yield Bytes::from(header);

            let mut file = File::open(&path).await?;
            file.seek(SeekFrom::Start(range.start)).await?;
            let mut reader_stream = ReaderStream::new(file.take(range.len()));
            while let Some(chunk) = reader_stream.next().await {
                yield chunk?;
            }
            yield Bytes::from_static(b"\r\n");
        }
        yield Bytes::from(format!("--{boundary_stream}--\r\n"));
    };
    let stream: Pin<Box<dyn futures_core::Stream<Item = Result<Bytes, std::io::Error>> + Send>> =
        Box::pin(stream);

    let mut builder = Response::builder()
        .status(StatusCode::PARTIAL_CONTENT)
        .header(ACCEPT_RANGES, "bytes")
        .header(CONTENT_TYPE, content_type)
        .header(ETAG, etag);

    if let Some(last_modified) = last_modified {
        builder = builder.header(LAST_MODIFIED, last_modified);
    }

    Ok(builder
        .body(Body::from_stream(stream))
        .map_err(|_| ApiError::Internal)?
        .into_response())
}

async fn resolve_disk_path(
    cfg: &Config,
    image_id: &str,
    headers: &HeaderMap,
    token_qs: Option<&str>,
) -> Result<PathBuf, ApiError> {
    if !is_safe_path_segment(image_id) {
        return Err(ApiError::BadRequest("invalid image id"));
    }

    let public_path = public_image_path(cfg, image_id);
    if tokio::fs::try_exists(&public_path)
        .await
        .map_err(|_| ApiError::Internal)?
    {
        return Ok(public_path);
    }

    let token = extract_token(headers, token_qs).ok_or(ApiError::Unauthorized)?;
    let claims = verify_lease(cfg, token)?;
    if claims.img != image_id {
        return Err(ApiError::Forbidden);
    }
    if !is_safe_path_segment(&claims.sub) {
        return Err(ApiError::Forbidden);
    }

    Ok(private_image_path(cfg, &claims.sub, image_id))
}

fn extract_token<'a>(headers: &'a HeaderMap, token_qs: Option<&'a str>) -> Option<&'a str> {
    if let Some(auth) = headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        if let Some(token) = auth.strip_prefix("Bearer ") {
            if !token.trim().is_empty() {
                return Some(token.trim());
            }
        }
    }
    token_qs.filter(|s| !s.trim().is_empty())
}

fn last_modified_header_value(last_modified: Option<SystemTime>) -> Option<HeaderValue> {
    let last_modified = last_modified?;
    // `httpdate::fmt_http_date` panics if the time is before the Unix epoch.
    //
    // While pre-epoch mtimes are rare in practice, they can happen (filesystem metadata, or
    // operator-specified values). Avoid crashing the server; omit the header instead.
    if last_modified.duration_since(UNIX_EPOCH).is_err() {
        return None;
    }
    let s = httpdate::fmt_http_date(last_modified);
    Some(HeaderValue::from_str(&s).expect("http-date must be a valid header value"))
}

/// Evaluates conditional request headers for `GET`/`HEAD`.
///
/// Precedence is per RFC 9110:
/// - If `If-None-Match` is present it dominates `If-Modified-Since`.
fn is_not_modified(
    req_headers: &HeaderMap,
    current_etag: Option<&str>,
    current_last_modified: Option<SystemTime>,
) -> bool {
    if let Some(inm) = req_headers.get(IF_NONE_MATCH) {
        let Some(current_etag) = current_etag else {
            return false;
        };
        let Ok(inm) = inm.to_str() else {
            return false;
        };
        return if_none_match_matches(inm, current_etag);
    }

    let Some(ims) = req_headers.get(IF_MODIFIED_SINCE) else {
        return false;
    };
    let Some(resource_last_modified) = current_last_modified else {
        return false;
    };
    let Ok(ims) = ims.to_str() else {
        return false;
    };
    let Ok(ims_time) = httpdate::parse_http_date(ims) else {
        return false;
    };

    // HTTP dates have 1-second resolution. Filesystems often provide sub-second mtimes, but our
    // `Last-Modified` header (and thus `If-Modified-Since`) cannot represent that. Compare at
    // second granularity to avoid false negatives where the resource's mtime has sub-second data
    // that gets truncated when formatting/parsing the HTTP date.
    let Ok(resource_secs) = resource_last_modified.duration_since(UNIX_EPOCH) else {
        return false;
    };
    let Ok(ims_secs) = ims_time.duration_since(UNIX_EPOCH) else {
        return false;
    };
    resource_secs.as_secs() <= ims_secs.as_secs()
}

fn not_modified_response(etag: HeaderValue, last_modified: Option<HeaderValue>) -> Response {
    let mut builder = Response::builder()
        .status(StatusCode::NOT_MODIFIED)
        .header(ACCEPT_RANGES, "bytes")
        .header(ETAG, etag)
        .header(CONTENT_LENGTH, "0");

    if let Some(last_modified) = last_modified {
        builder = builder.header(LAST_MODIFIED, last_modified);
    }

    builder
        .body(Body::empty())
        .unwrap_or_else(|_| StatusCode::NOT_MODIFIED.into_response())
}

fn if_none_match_matches(if_none_match: &str, current_etag: &str) -> bool {
    let current = strip_weak_prefix(current_etag.trim());

    // `If-None-Match` is a comma-separated list of entity-tags, but commas are allowed inside
    // a quoted entity-tag value. Split only on commas that occur *outside* quotes.
    let mut start = 0usize;
    let mut in_quotes = false;
    let bytes = if_none_match.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'"' => in_quotes = !in_quotes,
            b',' if !in_quotes => {
                let tag = if_none_match[start..i].trim();
                if tag == "*" {
                    return true;
                }
                if !tag.is_empty() && strip_weak_prefix(tag) == current {
                    return true;
                }
                start = i + 1;
            }
            _ => {}
        }
    }

    let tag = if_none_match[start..].trim();
    if tag == "*" {
        return true;
    }
    !tag.is_empty() && strip_weak_prefix(tag) == current

}

fn strip_weak_prefix(tag: &str) -> &str {
    let trimmed = tag.trim();
    trimmed
        .strip_prefix("W/")
        .or_else(|| trimmed.strip_prefix("w/"))
        .unwrap_or(trimmed)
}

fn if_range_allows_range(
    if_range: &str,
    current_etag: Option<&str>,
    current_last_modified: Option<SystemTime>,
) -> bool {
    let if_range = if_range.trim();

    // Entity-tag form. RFC 9110 requires strong comparison and disallows weak validators.
    if if_range.starts_with('"') || if_range.starts_with("W/") || if_range.starts_with("w/") {
        let Some(current_etag) = current_etag else {
            return false;
        };
        // If either side is weak, treat it as not matching for If-Range purposes.
        let current_etag = current_etag.trim_start();
        if if_range.starts_with("W/")
            || if_range.starts_with("w/")
            || current_etag.starts_with("W/")
            || current_etag.starts_with("w/")
        {
            return false;
        }
        return if_range == current_etag;
    }

    // HTTP-date form.
    let Ok(since) = httpdate::parse_http_date(if_range) else {
        return false;
    };
    let Some(last_modified) = current_last_modified else {
        return false;
    };

    // HTTP dates have 1-second resolution. Filesystems often provide sub-second mtimes, but our
    // `Last-Modified` header (and thus `If-Range` in HTTP-date form) cannot represent that.
    // Compare at second granularity to avoid false mismatches where the resource mtime has
    // sub-second data that gets truncated when formatting/parsing the HTTP date.
    let Ok(resource_secs) = last_modified.duration_since(UNIX_EPOCH) else {
        return false;
    };
    let Ok(since_secs) = since.duration_since(UNIX_EPOCH) else {
        return false;
    };
    resource_secs.as_secs() <= since_secs.as_secs()
}

fn compute_etag(metadata: &std::fs::Metadata) -> HeaderValue {
    let size = metadata.len();
    let modified = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);

    let tag = format!("\"{size}-{modified}\"");
    HeaderValue::from_str(&tag).unwrap_or_else(|_| HeaderValue::from_static("\"0-0\""))
}

fn range_not_satisfiable_response(size: u64) -> Response {
    let resp = Response::builder()
        .status(StatusCode::RANGE_NOT_SATISFIABLE)
        .header(ACCEPT_RANGES, "bytes")
        .header(CONTENT_RANGE, format!("bytes */{size}"))
        .header(CONTENT_LENGTH, "0")
        .body(Body::empty())
        .unwrap_or_else(|_| StatusCode::RANGE_NOT_SATISFIABLE.into_response());
    resp
}

fn payload_too_large_response() -> Response {
    Response::builder()
        .status(StatusCode::PAYLOAD_TOO_LARGE)
        .header(ACCEPT_RANGES, "bytes")
        .header(CONTENT_LENGTH, "0")
        .body(Body::empty())
        .unwrap_or_else(|_| StatusCode::PAYLOAD_TOO_LARGE.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use axum::http::Request;
    use http::header::ACCESS_CONTROL_REQUEST_HEADERS;
    use http::header::ACCESS_CONTROL_REQUEST_METHOD;
    use http::Method;
    use tokio::io::{AsyncSeekExt, AsyncWriteExt, SeekFrom};
    use tower::ServiceExt;

    const LARGE_FOUR_GIB: u64 = 4_294_967_296; // 2^32
    const LARGE_FILE_SIZE: u64 = LARGE_FOUR_GIB + 1024; // just over 4GiB (avoid a 5GiB sparse file in tests)
    const LARGE_HIGH_OFFSET: u64 = LARGE_FOUR_GIB + 123; // 2^32 + 123
    const LARGE_SENTINEL_HIGH: &[u8] = b"AERO_RANGE_4GB";
    const LARGE_SENTINEL_END: &[u8] = b"AERO_RANGE_END";

    fn test_config(public_dir: PathBuf, private_dir: PathBuf) -> Config {
        let mut allowed = HashSet::new();
        allowed.insert("https://app.example".to_owned());
        Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            public_dir,
            private_dir,
            token_secret: "test-secret".into(),
            cors_allowed_origins: AllowedOrigins::List(allowed),
            corp_policy: CorpPolicy::SameSite,
            lease_ttl: Duration::from_secs(60),
            max_ranges: 16,
            max_total_bytes: 512 * 1024 * 1024,
        }
    }

    async fn write_file(path: &Path, data: &[u8]) {
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(path, data).await.unwrap();
    }

    async fn write_sparse_test_image(path: &Path) {
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(true)
            .open(path)
            .await
            .unwrap();

        file.seek(SeekFrom::Start(LARGE_HIGH_OFFSET)).await.unwrap();
        file.write_all(LARGE_SENTINEL_HIGH).await.unwrap();

        let end_offset = LARGE_FILE_SIZE - (LARGE_SENTINEL_END.len() as u64);
        file.seek(SeekFrom::Start(end_offset)).await.unwrap();
        file.write_all(LARGE_SENTINEL_END).await.unwrap();

        file.flush().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn range_206_has_correct_headers_and_body() {
        let tmp = tempfile::tempdir().unwrap();
        let public_dir = tmp.path().join("public");
        let private_dir = tmp.path().join("private");
        let cfg = test_config(public_dir.clone(), private_dir);

        write_file(&public_image_path(&cfg, "win7"), b"abcdef").await;

        let app = app(cfg);
        let req = Request::builder()
            .method(Method::GET)
            .uri("/disk/win7")
            .header(RANGE, "bytes=1-3")
            .header(ORIGIN, "https://app.example")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            resp.headers()
                .get(ACCESS_CONTROL_ALLOW_ORIGIN)
                .unwrap()
                .to_str()
                .unwrap(),
            "https://app.example"
        );
        assert_eq!(
            resp.headers()
                .get("cross-origin-resource-policy")
                .unwrap()
                .to_str()
                .unwrap(),
            "same-site"
        );
        assert_eq!(
            resp.headers()
                .get(CONTENT_ENCODING)
                .unwrap()
                .to_str()
                .unwrap(),
            "identity"
        );
        assert_eq!(
            resp.headers().get(CONTENT_RANGE).unwrap().to_str().unwrap(),
            "bytes 1-3/6"
        );
        assert_eq!(
            resp.headers()
                .get(CONTENT_LENGTH)
                .unwrap()
                .to_str()
                .unwrap(),
            "3"
        );

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"bcd");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn get_without_range_returns_200_with_full_body() {
        let tmp = tempfile::tempdir().unwrap();
        let public_dir = tmp.path().join("public");
        let private_dir = tmp.path().join("private");
        let cfg = test_config(public_dir.clone(), private_dir);
        write_file(&public_image_path(&cfg, "win7"), b"abcdef").await;

        let app = app(cfg);
        let req = Request::builder()
            .method(Method::GET)
            .uri("/disk/win7")
            .header(ORIGIN, "https://app.example")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(ACCEPT_RANGES).unwrap().to_str().unwrap(),
            "bytes"
        );
        assert_eq!(
            resp.headers()
                .get(CONTENT_LENGTH)
                .unwrap()
                .to_str()
                .unwrap(),
            "6"
        );
        assert_eq!(
            resp.headers().get(CONTENT_TYPE).unwrap().to_str().unwrap(),
            "application/octet-stream"
        );
        assert_eq!(
            resp.headers().get(CACHE_CONTROL).unwrap().to_str().unwrap(),
            "no-transform"
        );

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"abcdef");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn get_if_none_match_returns_304() {
        let tmp = tempfile::tempdir().unwrap();
        let public_dir = tmp.path().join("public");
        let private_dir = tmp.path().join("private");
        let cfg = test_config(public_dir.clone(), private_dir);

        write_file(&public_image_path(&cfg, "win7"), b"abcdef").await;
        let meta = tokio::fs::metadata(&public_image_path(&cfg, "win7"))
            .await
            .unwrap();
        let etag = compute_etag(&meta);

        let app = app(cfg);
        let req = Request::builder()
            .method(Method::GET)
            .uri("/disk/win7")
            .header(ORIGIN, "https://app.example")
            .header("if-none-match", etag.to_str().unwrap())
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(body.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn get_if_range_match_returns_206() {
        let tmp = tempfile::tempdir().unwrap();
        let public_dir = tmp.path().join("public");
        let private_dir = tmp.path().join("private");
        let cfg = test_config(public_dir.clone(), private_dir);

        write_file(&public_image_path(&cfg, "win7"), b"abcdef").await;
        let meta = tokio::fs::metadata(&public_image_path(&cfg, "win7"))
            .await
            .unwrap();
        let etag = compute_etag(&meta);

        let app = app(cfg);
        let req = Request::builder()
            .method(Method::GET)
            .uri("/disk/win7")
            .header(RANGE, "bytes=1-3")
            .header("if-range", etag.to_str().unwrap())
            .header(ORIGIN, "https://app.example")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"bcd");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn get_if_range_mismatch_ignores_range_and_returns_200() {
        let tmp = tempfile::tempdir().unwrap();
        let public_dir = tmp.path().join("public");
        let private_dir = tmp.path().join("private");
        let cfg = test_config(public_dir.clone(), private_dir);

        write_file(&public_image_path(&cfg, "win7"), b"abcdef").await;

        let app = app(cfg);
        let req = Request::builder()
            .method(Method::GET)
            .uri("/disk/win7")
            .header(RANGE, "bytes=1-3")
            .header("if-range", "\"mismatch\"")
            .header(ORIGIN, "https://app.example")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"abcdef");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn get_if_modified_since_returns_304() {
        let tmp = tempfile::tempdir().unwrap();
        let public_dir = tmp.path().join("public");
        let private_dir = tmp.path().join("private");
        let cfg = test_config(public_dir.clone(), private_dir);
        write_file(&public_image_path(&cfg, "win7"), b"abcdef").await;

        let meta = tokio::fs::metadata(&public_image_path(&cfg, "win7"))
            .await
            .unwrap();
        let modified = meta.modified().unwrap();
        let http_date = httpdate::fmt_http_date(modified);

        let app = app(cfg);
        let req = Request::builder()
            .method(Method::GET)
            .uri("/disk/win7")
            .header(ORIGIN, "https://app.example")
            .header(IF_MODIFIED_SINCE, http_date.clone())
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(
            resp.headers()
                .get(LAST_MODIFIED)
                .unwrap()
                .to_str()
                .unwrap(),
            http_date
        );

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(body.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn if_none_match_dominates_if_modified_since() {
        let tmp = tempfile::tempdir().unwrap();
        let public_dir = tmp.path().join("public");
        let private_dir = tmp.path().join("private");
        let cfg = test_config(public_dir.clone(), private_dir);
        write_file(&public_image_path(&cfg, "win7"), b"abcdef").await;

        let meta = tokio::fs::metadata(&public_image_path(&cfg, "win7"))
            .await
            .unwrap();
        let modified = meta.modified().unwrap();
        let http_date = httpdate::fmt_http_date(modified);

        let app = app(cfg);
        let req = Request::builder()
            .method(Method::GET)
            .uri("/disk/win7")
            .header(ORIGIN, "https://app.example")
            .header(IF_NONE_MATCH, "\"mismatch\"")
            .header(IF_MODIFIED_SINCE, http_date)
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"abcdef");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn get_if_range_http_date_match_returns_206() {
        let tmp = tempfile::tempdir().unwrap();
        let public_dir = tmp.path().join("public");
        let private_dir = tmp.path().join("private");
        let cfg = test_config(public_dir.clone(), private_dir);
        write_file(&public_image_path(&cfg, "win7"), b"abcdef").await;

        let meta = tokio::fs::metadata(&public_image_path(&cfg, "win7"))
            .await
            .unwrap();
        let modified = meta.modified().unwrap();
        let http_date = httpdate::fmt_http_date(modified);

        let app = app(cfg);
        let req = Request::builder()
            .method(Method::GET)
            .uri("/disk/win7")
            .header(RANGE, "bytes=1-3")
            .header(IF_RANGE, http_date)
            .header(ORIGIN, "https://app.example")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"bcd");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn get_if_range_http_date_mismatch_ignores_range_and_returns_200() {
        let tmp = tempfile::tempdir().unwrap();
        let public_dir = tmp.path().join("public");
        let private_dir = tmp.path().join("private");
        let cfg = test_config(public_dir.clone(), private_dir);
        write_file(&public_image_path(&cfg, "win7"), b"abcdef").await;

        let meta = tokio::fs::metadata(&public_image_path(&cfg, "win7"))
            .await
            .unwrap();
        let modified = meta.modified().unwrap();
        let old = modified
            .checked_sub(Duration::from_secs(60))
            .expect("mtime should be far after the unix epoch");
        let http_date = httpdate::fmt_http_date(old);

        let app = app(cfg);
        let req = Request::builder()
            .method(Method::GET)
            .uri("/disk/win7")
            .header(RANGE, "bytes=1-3")
            .header(IF_RANGE, http_date)
            .header(ORIGIN, "https://app.example")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"abcdef");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn head_without_range_returns_headers_only() {
        let tmp = tempfile::tempdir().unwrap();
        let public_dir = tmp.path().join("public");
        let private_dir = tmp.path().join("private");
        let cfg = test_config(public_dir.clone(), private_dir);
        write_file(&public_image_path(&cfg, "win7"), b"abcdef").await;

        let app = app(cfg);
        let req = Request::builder()
            .method(Method::HEAD)
            .uri("/disk/win7")
            .header(ORIGIN, "https://app.example")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(ACCEPT_RANGES).unwrap().to_str().unwrap(),
            "bytes"
        );
        assert_eq!(
            resp.headers()
                .get(CONTENT_LENGTH)
                .unwrap()
                .to_str()
                .unwrap(),
            "6"
        );
        assert_eq!(
            resp.headers().get(CONTENT_TYPE).unwrap().to_str().unwrap(),
            "application/octet-stream"
        );
        assert_eq!(
            resp.headers().get(CACHE_CONTROL).unwrap().to_str().unwrap(),
            "no-transform"
        );

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(body.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn head_if_none_match_returns_304() {
        let tmp = tempfile::tempdir().unwrap();
        let public_dir = tmp.path().join("public");
        let private_dir = tmp.path().join("private");
        let cfg = test_config(public_dir.clone(), private_dir);
        write_file(&public_image_path(&cfg, "win7"), b"abcdef").await;

        let meta = tokio::fs::metadata(&public_image_path(&cfg, "win7"))
            .await
            .unwrap();
        let etag = compute_etag(&meta);

        let app = app(cfg);
        let req = Request::builder()
            .method(Method::HEAD)
            .uri("/disk/win7")
            .header(ORIGIN, "https://app.example")
            .header("if-none-match", etag.to_str().unwrap())
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(body.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn head_if_modified_since_returns_304() {
        let tmp = tempfile::tempdir().unwrap();
        let public_dir = tmp.path().join("public");
        let private_dir = tmp.path().join("private");
        let cfg = test_config(public_dir.clone(), private_dir);
        write_file(&public_image_path(&cfg, "win7"), b"abcdef").await;

        let meta = tokio::fs::metadata(&public_image_path(&cfg, "win7"))
            .await
            .unwrap();
        let modified = meta.modified().unwrap();
        let http_date = httpdate::fmt_http_date(modified);

        let app = app(cfg);
        let req = Request::builder()
            .method(Method::HEAD)
            .uri("/disk/win7")
            .header(ORIGIN, "https://app.example")
            .header(IF_MODIFIED_SINCE, http_date.clone())
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(
            resp.headers()
                .get(LAST_MODIFIED)
                .unwrap()
                .to_str()
                .unwrap(),
            http_date
        );

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(body.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn head_range_206_has_headers_only() {
        let tmp = tempfile::tempdir().unwrap();
        let public_dir = tmp.path().join("public");
        let private_dir = tmp.path().join("private");
        let cfg = test_config(public_dir.clone(), private_dir);

        write_file(&public_image_path(&cfg, "win7"), b"abcdef").await;

        let app = app(cfg);
        let req = Request::builder()
            .method(Method::HEAD)
            .uri("/disk/win7")
            .header(RANGE, "bytes=1-3")
            .header(ORIGIN, "https://app.example")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            resp.headers().get(CONTENT_RANGE).unwrap().to_str().unwrap(),
            "bytes 1-3/6"
        );
        assert_eq!(
            resp.headers()
                .get(CONTENT_LENGTH)
                .unwrap()
                .to_str()
                .unwrap(),
            "3"
        );

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(body.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn multi_range_returns_multipart_206() {
        let tmp = tempfile::tempdir().unwrap();
        let public_dir = tmp.path().join("public");
        let private_dir = tmp.path().join("private");
        let cfg = test_config(public_dir.clone(), private_dir);

        write_file(&public_image_path(&cfg, "win7"), b"abcdef").await;

        let app = app(cfg);
        let req = Request::builder()
            .method(Method::GET)
            .uri("/disk/win7")
            .header(RANGE, "bytes=0-0,2-2")
            .header(ORIGIN, "https://app.example")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);

        let content_type = resp
            .headers()
            .get(CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(content_type.starts_with("multipart/byteranges; boundary="));
        let boundary = content_type.split("boundary=").nth(1).unwrap();

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();

        let expected = format!(
            "--{b}\r\nContent-Type: application/octet-stream\r\nContent-Range: bytes 0-0/6\r\n\r\na\r\n--{b}\r\nContent-Type: application/octet-stream\r\nContent-Range: bytes 2-2/6\r\n\r\nc\r\n--{b}--\r\n",
            b = boundary
        );
        assert_eq!(body_str, expected);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn multi_range_abuse_guard_returns_413() {
        let tmp = tempfile::tempdir().unwrap();
        let public_dir = tmp.path().join("public");
        let private_dir = tmp.path().join("private");
        let mut cfg = test_config(public_dir.clone(), private_dir);
        cfg.max_ranges = 1;

        write_file(&public_image_path(&cfg, "win7"), b"abcdef").await;

        let app = app(cfg);
        let req = Request::builder()
            .method(Method::GET)
            .uri("/disk/win7")
            .header(RANGE, "bytes=0-0,2-2")
            .header(ORIGIN, "https://app.example")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn max_total_bytes_guard_returns_413() {
        let tmp = tempfile::tempdir().unwrap();
        let public_dir = tmp.path().join("public");
        let private_dir = tmp.path().join("private");
        let mut cfg = test_config(public_dir.clone(), private_dir);
        cfg.max_total_bytes = 2;

        write_file(&public_image_path(&cfg, "win7"), b"abcdef").await;

        let app = app(cfg);
        let req = Request::builder()
            .method(Method::GET)
            .uri("/disk/win7")
            .header(RANGE, "bytes=0-2")
            .header(ORIGIN, "https://app.example")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn range_416_has_content_range_star() {
        let tmp = tempfile::tempdir().unwrap();
        let public_dir = tmp.path().join("public");
        let private_dir = tmp.path().join("private");
        let cfg = test_config(public_dir.clone(), private_dir);

        write_file(&public_image_path(&cfg, "win7"), b"abcdef").await;

        let app = app(cfg);
        let req = Request::builder()
            .method(Method::GET)
            .uri("/disk/win7")
            .header(RANGE, "bytes=10-12")
            .header(ORIGIN, "https://app.example")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(
            resp.headers()
                .get("cross-origin-resource-policy")
                .unwrap()
                .to_str()
                .unwrap(),
            "same-site"
        );
        assert_eq!(
            resp.headers().get(CONTENT_RANGE).unwrap().to_str().unwrap(),
            "bytes */6"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn range_supports_offsets_beyond_4gib_and_suffix_ranges() {
        let tmp = tempfile::tempdir().unwrap();
        let public_dir = tmp.path().join("public");
        let private_dir = tmp.path().join("private");
        let cfg = test_config(public_dir.clone(), private_dir);

        write_sparse_test_image(&public_image_path(&cfg, "win7")).await;

        let app = app(cfg);

        // Explicit range starting beyond 2^32.
        let high_end = LARGE_HIGH_OFFSET + LARGE_SENTINEL_HIGH.len() as u64 - 1;
        let req = Request::builder()
            .method(Method::GET)
            .uri("/disk/win7")
            .header(RANGE, format!("bytes={}-{}", LARGE_HIGH_OFFSET, high_end))
            .header(ORIGIN, "https://app.example")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            resp.headers().get(CONTENT_RANGE).unwrap().to_str().unwrap(),
            format!("bytes {}-{}/{}", LARGE_HIGH_OFFSET, high_end, LARGE_FILE_SIZE)
        );
        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        assert_eq!(&body[..], LARGE_SENTINEL_HIGH);

        // Suffix range on a file > 4 GiB.
        let suffix_len = LARGE_SENTINEL_END.len();
        let req = Request::builder()
            .method(Method::GET)
            .uri("/disk/win7")
            .header(RANGE, format!("bytes=-{suffix_len}"))
            .header(ORIGIN, "https://app.example")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);

        let suffix_start = LARGE_FILE_SIZE - suffix_len as u64;
        let suffix_end = LARGE_FILE_SIZE - 1;
        assert_eq!(
            resp.headers().get(CONTENT_RANGE).unwrap().to_str().unwrap(),
            format!("bytes {suffix_start}-{suffix_end}/{LARGE_FILE_SIZE}")
        );

        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        assert_eq!(&body[..], LARGE_SENTINEL_END);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cors_preflight_includes_range_and_authorization() {
        let tmp = tempfile::tempdir().unwrap();
        let public_dir = tmp.path().join("public");
        let private_dir = tmp.path().join("private");
        let cfg = test_config(public_dir, private_dir);

        let app = app(cfg);
        let req = Request::builder()
            .method(Method::OPTIONS)
            .uri("/disk/win7")
            .header(ORIGIN, "https://app.example")
            .header(ACCESS_CONTROL_REQUEST_METHOD, "GET")
            .header(ACCESS_CONTROL_REQUEST_HEADERS, "range, authorization")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            resp.headers()
                .get(ACCESS_CONTROL_ALLOW_METHODS)
                .unwrap()
                .to_str()
                .unwrap(),
            "GET, HEAD, OPTIONS"
        );
        assert_eq!(
            resp.headers()
                .get(ACCESS_CONTROL_MAX_AGE)
                .unwrap()
                .to_str()
                .unwrap(),
            "86400"
        );
        let allow_headers = resp
            .headers()
            .get(ACCESS_CONTROL_ALLOW_HEADERS)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(allow_headers.contains("Range"));
        assert!(allow_headers.contains("Authorization"));
        assert!(allow_headers.contains("If-Range"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn private_image_requires_token() {
        let tmp = tempfile::tempdir().unwrap();
        let public_dir = tmp.path().join("public");
        let private_dir = tmp.path().join("private");
        let cfg = test_config(public_dir, private_dir.clone());

        write_file(&private_image_path(&cfg, "alice", "secret"), b"topsecret").await;

        let app = app(cfg);
        let req = Request::builder()
            .method(Method::GET)
            .uri("/disk/secret")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            resp.headers()
                .get("cross-origin-resource-policy")
                .unwrap()
                .to_str()
                .unwrap(),
            "same-site"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn private_image_denies_bad_token() {
        let tmp = tempfile::tempdir().unwrap();
        let public_dir = tmp.path().join("public");
        let private_dir = tmp.path().join("private");
        let cfg = test_config(public_dir, private_dir.clone());

        write_file(&private_image_path(&cfg, "alice", "secret"), b"topsecret").await;

        let app = app(cfg);
        let req = Request::builder()
            .method(Method::GET)
            .uri("/disk/secret")
            .header(AUTHORIZATION, "Bearer definitely-not-a-jwt")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            resp.headers()
                .get("cross-origin-resource-policy")
                .unwrap()
                .to_str()
                .unwrap(),
            "same-site"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn private_image_allows_valid_token() {
        let tmp = tempfile::tempdir().unwrap();
        let public_dir = tmp.path().join("public");
        let private_dir = tmp.path().join("private");
        let cfg = test_config(public_dir, private_dir.clone());

        write_file(&private_image_path(&cfg, "alice", "secret"), b"topsecret").await;

        let app = app(cfg.clone());
        let lease_req = Request::builder()
            .method(Method::POST)
            .uri("/api/images/secret/lease")
            .header("X-Debug-User", "alice")
            .body(Body::empty())
            .unwrap();
        let lease_resp = app.clone().oneshot(lease_req).await.unwrap();
        assert_eq!(lease_resp.status(), StatusCode::OK);
        let lease_body = axum::body::to_bytes(lease_resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let lease_json: serde_json::Value = serde_json::from_slice(&lease_body).unwrap();
        let token = lease_json
            .get("token")
            .and_then(|v| v.as_str())
            .unwrap()
            .to_owned();

        let disk_req = Request::builder()
            .method(Method::GET)
            .uri("/disk/secret")
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .header(RANGE, "bytes=0-2")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(disk_req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"top");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rejects_dotdot_image_id() {
        let tmp = tempfile::tempdir().unwrap();
        let public_dir = tmp.path().join("public");
        let private_dir = tmp.path().join("private");
        let cfg = test_config(public_dir, private_dir);
        let app = app(cfg);

        let req = Request::builder()
            .method(Method::GET)
            .uri("/disk/..")
            .header(ORIGIN, "https://app.example")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            resp.headers()
                .get("cross-origin-resource-policy")
                .unwrap()
                .to_str()
                .unwrap(),
            "same-site"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rejects_dotdot_user_id() {
        let tmp = tempfile::tempdir().unwrap();
        let public_dir = tmp.path().join("public");
        let private_dir = tmp.path().join("private");
        let cfg = test_config(public_dir, private_dir);
        let app = app(cfg);

        let req = Request::builder()
            .method(Method::POST)
            .uri("/api/images/secret/lease")
            .header(ORIGIN, "https://app.example")
            .header(AUTHORIZATION, "Bearer ..")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn parse_range_header_tolerates_whitespace() {
        let specs = parse_range_header("bytes =\t 1 - 3").unwrap();
        let resolved = resolve_ranges(&specs, 10, false).unwrap();
        assert_eq!(
            resolved,
            vec![ResolvedByteRange {
                start: 1,
                end: 3
            }]
        );
    }

    #[test]
    fn if_none_match_handles_commas_inside_etag() {
        assert!(if_none_match_matches("\"a,b\"", "\"a,b\""));
        assert!(if_none_match_matches("W/\"x\", \"a,b\"", "\"a,b\""));
        assert!(!if_none_match_matches("\"a,b\"", "\"c\""));
    }

    #[test]
    fn if_modified_since_ignores_subsecond_precision() {
        let mut headers = HeaderMap::new();
        let last_modified = UNIX_EPOCH + Duration::from_secs(123) + Duration::from_nanos(456);
        let http_date = httpdate::fmt_http_date(last_modified);
        headers.insert(
            IF_MODIFIED_SINCE,
            HeaderValue::from_str(&http_date).unwrap(),
        );

        assert!(
            is_not_modified(&headers, None, Some(last_modified)),
            "expected If-Modified-Since to match even when the resource mtime has sub-second precision"
        );
    }

    #[test]
    fn last_modified_header_value_does_not_panic_for_pre_epoch_times() {
        let t = UNIX_EPOCH - Duration::from_secs(1);
        assert!(last_modified_header_value(Some(t)).is_none());
    }

    #[test]
    fn if_range_http_date_ignores_subsecond_precision() {
        let last_modified = UNIX_EPOCH + Duration::from_secs(123) + Duration::from_nanos(456);
        let http_date = httpdate::fmt_http_date(last_modified);

        assert!(
            if_range_allows_range(&http_date, None, Some(last_modified)),
            "expected If-Range date to match even when the resource mtime has sub-second precision"
        );
    }

    #[test]
    fn apply_cors_headers_appends_vary_instead_of_overwriting() {
        let mut allowed = HashSet::new();
        allowed.insert("https://app.example".to_owned());
        let cfg = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            public_dir: PathBuf::from("public"),
            private_dir: PathBuf::from("private"),
            token_secret: "secret".into(),
            cors_allowed_origins: AllowedOrigins::List(allowed),
            corp_policy: CorpPolicy::SameSite,
            lease_ttl: Duration::from_secs(60),
            max_ranges: 16,
            max_total_bytes: 512 * 1024 * 1024,
        };

        let origin = HeaderValue::from_static("https://app.example");

        let mut resp = StatusCode::OK.into_response();
        resp.headers_mut()
            .insert(VARY, HeaderValue::from_static("Accept-Encoding"));

        apply_cors_headers(&cfg, Some(&origin), &mut resp, false, "");

        assert_eq!(
            resp.headers().get(VARY).unwrap().to_str().unwrap(),
            "Accept-Encoding, Origin"
        );
    }

    #[test]
    fn apply_cors_headers_preserves_vary_star() {
        let cfg = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            public_dir: PathBuf::from("public"),
            private_dir: PathBuf::from("private"),
            token_secret: "secret".into(),
            cors_allowed_origins: AllowedOrigins::Any,
            corp_policy: CorpPolicy::SameSite,
            lease_ttl: Duration::from_secs(60),
            max_ranges: 16,
            max_total_bytes: 512 * 1024 * 1024,
        };

        let origin = HeaderValue::from_static("https://app.example");

        let mut resp = StatusCode::NO_CONTENT.into_response();
        resp.headers_mut().insert(VARY, HeaderValue::from_static("*"));

        apply_cors_headers(
            &cfg,
            Some(&origin),
            &mut resp,
            true,
            "GET, HEAD, OPTIONS",
        );

        assert_eq!(resp.headers().get(VARY).unwrap().to_str().unwrap(), "*");
    }

    #[test]
    fn verify_lease_rejects_non_canonical_base64url_segments() {
        let cfg = test_config(PathBuf::from("public"), PathBuf::from("private"));

        let signature_ok = "A".repeat(HMAC_SHA256_SIG_B64URL_LEN);

        // Header has len%4==2, but last char has non-zero unused bits => non-canonical.
        let token = format!("AB.AA.{signature_ok}");
        let err = verify_lease(&cfg, &token).unwrap_err();
        assert!(matches!(err, ApiError::Unauthorized));

        // Payload has len%4==2, but last char has non-zero unused bits => non-canonical.
        let token = format!("AA.AB.{signature_ok}");
        let err = verify_lease(&cfg, &token).unwrap_err();
        assert!(matches!(err, ApiError::Unauthorized));

        // Signature has len%4==3, but last char has non-zero unused bits => non-canonical.
        let signature_bad = format!("{}B", "A".repeat(HMAC_SHA256_SIG_B64URL_LEN - 1));
        let token = format!("AA.AA.{signature_bad}");
        let err = verify_lease(&cfg, &token).unwrap_err();
        assert!(matches!(err, ApiError::Unauthorized));
    }
}
