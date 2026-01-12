#![forbid(unsafe_code)]

use std::collections::HashSet;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use async_stream::try_stream;
use axum::body::Body;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::header::{
    ACCEPT_RANGES, ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS,
    ACCESS_CONTROL_ALLOW_ORIGIN, ACCESS_CONTROL_EXPOSE_HEADERS, ACCESS_CONTROL_MAX_AGE,
    AUTHORIZATION, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG, IF_NONE_MATCH,
    IF_RANGE, ORIGIN, RANGE, VARY,
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

#[derive(Clone, Debug)]
pub struct Config {
    pub bind: String,
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
        let bind = std::env::var("DISK_GATEWAY_BIND").unwrap_or_else(|_| "127.0.0.1:3000".into());
        let public_dir =
            std::env::var("DISK_GATEWAY_PUBLIC_DIR").unwrap_or_else(|_| "./public-images".into());
        let private_dir =
            std::env::var("DISK_GATEWAY_PRIVATE_DIR").unwrap_or_else(|_| "./private-images".into());
        let token_secret = std::env::var("DISK_GATEWAY_TOKEN_SECRET")
            .map_err(|_| ConfigError::MissingEnv("DISK_GATEWAY_TOKEN_SECRET"))?;
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
            public_dir: PathBuf::from(public_dir),
            private_dir: PathBuf::from(private_dir),
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

#[derive(Clone, Debug)]
pub enum AllowedOrigins {
    Any,
    List(HashSet<String>),
}

impl AllowedOrigins {
    fn from_env() -> Result<Self, ConfigError> {
        let raw = std::env::var("DISK_GATEWAY_CORS_ALLOWED_ORIGINS").unwrap_or_else(|_| "*".into());
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
        let raw = std::env::var("DISK_GATEWAY_CORP").unwrap_or_else(|_| "same-site".into());
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
            resp.headers_mut()
                .insert(VARY, HeaderValue::from_static("Origin"));
        }

        resp.headers_mut().insert(
            ACCESS_CONTROL_EXPOSE_HEADERS,
            HeaderValue::from_static("Accept-Ranges, Content-Range, Content-Length, ETag, Last-Modified"),
        );
    }

    if is_preflight {
        // Even when `Access-Control-Allow-Origin: *`, varying on the preflight request headers is a
        // safe default for caches and avoids surprising behavior if deployments later move to an
        // allowlist.
        resp.headers_mut().insert(
            VARY,
            HeaderValue::from_static("Origin, Access-Control-Request-Method, Access-Control-Request-Headers"),
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

    let etag = compute_etag(&metadata);
    let etag_str = etag.to_str().ok();

    if let (Some(etag_str), Some(if_none_match)) = (
        etag_str,
        headers.get(IF_NONE_MATCH).and_then(|v| v.to_str().ok()),
    ) {
        if if_none_match_matches(if_none_match, etag_str) {
            return Ok(not_modified_response(etag.clone()));
        }
    }
    let range_header = headers.get(RANGE).and_then(|v| v.to_str().ok());

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
    let etag = compute_etag(&metadata);

    let etag_str = etag.to_str().ok();

    // Conditional requests: If-None-Match dominates If-Modified-Since (we only implement ETag
    // revalidation here).
    if let (Some(etag_str), Some(if_none_match)) = (
        etag_str,
        headers.get(IF_NONE_MATCH).and_then(|v| v.to_str().ok()),
    ) {
        if if_none_match_matches(if_none_match, etag_str) {
            return Ok(not_modified_response(etag.clone()));
        }
    }

    let mut range_header = headers.get(RANGE).and_then(|v| v.to_str().ok());
    let if_range = headers.get(IF_RANGE).and_then(|v| v.to_str().ok());
    if let (Some(_range), Some(if_range), Some(etag_str)) = (range_header, if_range, etag_str) {
        if !if_range_allows_range(if_range, etag_str) {
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
        None => serve_full_file(&path, size, etag).await,
        Some(ranges) if ranges.len() == 1 => serve_single_range(&path, size, etag, ranges[0]).await,
        Some(ranges) => serve_multi_range(&path, size, etag, ranges).await,
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
) -> Result<Response, ApiError> {
    let file = File::open(path).await.map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => ApiError::NotFound,
        _ => ApiError::Internal,
    })?;

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(ACCEPT_RANGES, "bytes")
        .header(CONTENT_TYPE, "application/octet-stream")
        .header(CONTENT_LENGTH, size)
        .header(ETAG, etag)
        .body(Body::from_stream(ReaderStream::new(file)))
        .map_err(|_| ApiError::Internal)?
        .into_response())
}

async fn serve_single_range(
    path: &PathBuf,
    size: u64,
    etag: HeaderValue,
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

    Ok(Response::builder()
        .status(StatusCode::PARTIAL_CONTENT)
        .header(ACCEPT_RANGES, "bytes")
        .header(CONTENT_TYPE, "application/octet-stream")
        .header(
            CONTENT_RANGE,
            format!("bytes {}-{}/{}", range.start, range.end, size),
        )
        .header(CONTENT_LENGTH, len)
        .header(ETAG, etag)
        .body(Body::from_stream(ReaderStream::new(file.take(len))))
        .map_err(|_| ApiError::Internal)?
        .into_response())
}

async fn serve_multi_range(
    path: &PathBuf,
    size: u64,
    etag: HeaderValue,
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

    Ok(Response::builder()
        .status(StatusCode::PARTIAL_CONTENT)
        .header(ACCEPT_RANGES, "bytes")
        .header(CONTENT_TYPE, content_type)
        .header(ETAG, etag)
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

fn not_modified_response(etag: HeaderValue) -> Response {
    Response::builder()
        .status(StatusCode::NOT_MODIFIED)
        .header(ACCEPT_RANGES, "bytes")
        .header(ETAG, etag)
        .header(CONTENT_LENGTH, "0")
        .body(Body::empty())
        .unwrap_or_else(|_| StatusCode::NOT_MODIFIED.into_response())
}

fn if_none_match_matches(if_none_match: &str, current_etag: &str) -> bool {
    let current = strip_weak_prefix(current_etag.trim());

    for raw in if_none_match.split(',') {
        let tag = raw.trim();
        if tag == "*" {
            return true;
        }
        if strip_weak_prefix(tag) == current {
            return true;
        }
    }

    false
}

fn strip_weak_prefix(tag: &str) -> &str {
    let trimmed = tag.trim();
    trimmed
        .strip_prefix("W/")
        .or_else(|| trimmed.strip_prefix("w/"))
        .unwrap_or(trimmed)
}

fn if_range_allows_range(if_range: &str, current_etag: &str) -> bool {
    let if_range = if_range.trim();
    let current_etag = current_etag.trim();

    // Only implement the entity-tag form here. RFC 9110 requires strong comparison.
    if !(if_range.starts_with('"')
        || if_range.starts_with("W/")
        || if_range.starts_with("w/"))
    {
        return false;
    }

    if if_range.starts_with("W/")
        || if_range.starts_with("w/")
        || current_etag.starts_with("W/")
        || current_etag.starts_with("w/")
    {
        return false;
    }

    if_range == current_etag
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
            bind: "127.0.0.1:0".into(),
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
}
