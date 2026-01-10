#![forbid(unsafe_code)]

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::header::{
    ACCEPT_RANGES, ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS,
    ACCESS_CONTROL_ALLOW_ORIGIN, ACCESS_CONTROL_EXPOSE_HEADERS, ACCESS_CONTROL_MAX_AGE,
    AUTHORIZATION, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG, ORIGIN, RANGE, VARY,
};
use axum::http::{HeaderMap, HeaderValue, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, options, post};
use axum::Router;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
use tokio_util::io::ReaderStream;

#[derive(Clone, Debug)]
pub struct Config {
    pub bind: String,
    pub public_dir: PathBuf,
    pub private_dir: PathBuf,
    pub token_secret: String,
    pub cors_allowed_origins: AllowedOrigins,
    pub corp_policy: CorpPolicy,
    pub lease_ttl: Duration,
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let bind = std::env::var("DISK_GATEWAY_BIND").unwrap_or_else(|_| "127.0.0.1:3000".into());
        let public_dir = std::env::var("DISK_GATEWAY_PUBLIC_DIR")
            .unwrap_or_else(|_| "./public-images".into());
        let private_dir = std::env::var("DISK_GATEWAY_PRIVATE_DIR")
            .unwrap_or_else(|_| "./private-images".into());
        let token_secret = std::env::var("DISK_GATEWAY_TOKEN_SECRET")
            .map_err(|_| ConfigError::MissingEnv("DISK_GATEWAY_TOKEN_SECRET"))?;
        let cors_allowed_origins = AllowedOrigins::from_env()?;
        let corp_policy = CorpPolicy::from_env()?;
        let lease_ttl = std::env::var("DISK_GATEWAY_LEASE_TTL_SECONDS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or_else(|| Duration::from_secs(15 * 60));

        Ok(Self {
            bind,
            public_dir: PathBuf::from(public_dir),
            private_dir: PathBuf::from(private_dir),
            token_secret,
            cors_allowed_origins,
            corp_policy,
            lease_ttl,
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
        .route(
            "/:id",
            get(disk_get).head(disk_head).options(disk_options),
        )
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
    let origin = req.headers().get(ORIGIN).cloned();
    let mut resp = next.run(req).await;
    apply_cors_headers(&state.cfg, origin.as_ref(), &mut resp, false, "");
    resp.headers_mut().insert(
        HeaderName::from_static("cross-origin-resource-policy"),
        state.cfg.corp_policy.as_header_value(),
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
            resp.headers_mut().insert(VARY, HeaderValue::from_static("Origin"));
        }

        resp.headers_mut().insert(
            ACCESS_CONTROL_EXPOSE_HEADERS,
            HeaderValue::from_static("Accept-Ranges, Content-Range, Content-Length, ETag"),
        );
    }

    if is_preflight {
        resp.headers_mut().insert(
            ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static(allow_methods),
        );
        resp.headers_mut().insert(
            ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static("Range, If-Range, Authorization, Content-Type"),
        );
        resp.headers_mut()
            .insert(ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("86400"));
    }
}

fn is_safe_path_segment(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
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

fn sign_lease(cfg: &Config, image_id: &str, user_id: &str, expires_at: OffsetDateTime) -> Result<String, ApiError> {
    let claims = LeaseClaims {
        img: image_id.to_owned(),
        sub: user_id.to_owned(),
        scope: "disk:read".to_owned(),
        exp: expires_at.unix_timestamp().try_into().map_err(|_| ApiError::Internal)?,
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
        let expires_at_str = expires_at.format(&Rfc3339).map_err(|_| ApiError::Internal)?;
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

    let expires_at = OffsetDateTime::now_utc() + time::Duration::try_from(state.cfg.lease_ttl).map_err(|_| ApiError::Internal)?;
    let token = sign_lease(&state.cfg, &image_id, user_id, expires_at)?;
    let expires_at_str = expires_at.format(&Rfc3339).map_err(|_| ApiError::Internal)?;
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
    let metadata = tokio::fs::metadata(&path).await.map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => ApiError::NotFound,
        _ => ApiError::Internal,
    })?;
    let size = metadata.len();

    let etag = compute_etag(&metadata);
    let mut resp = StatusCode::OK.into_response();
    resp.headers_mut()
        .insert(CONTENT_LENGTH, HeaderValue::from(size));
    resp.headers_mut()
        .insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
    resp.headers_mut().insert(ETAG, etag);

    Ok(resp)
}

async fn disk_get(
    State(state): State<Arc<AppState>>,
    AxumPath(image_id): AxumPath<String>,
    Query(query): Query<TokenQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let path = resolve_disk_path(&state.cfg, &image_id, &headers, query.token.as_deref()).await?;
    let metadata = tokio::fs::metadata(&path).await.map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => ApiError::NotFound,
        _ => ApiError::Internal,
    })?;
    let size = metadata.len();
    let etag = compute_etag(&metadata);

    let range_header = headers.get(RANGE).and_then(|v| v.to_str().ok());
    let mut file = File::open(&path).await.map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => ApiError::NotFound,
        _ => ApiError::Internal,
    })?;

    let (status, content_range, stream_len) = if let Some(range_header) = range_header {
        match parse_single_range(range_header, size) {
            Ok(Some((start, end))) => {
                let len = end - start + 1;
                file.seek(SeekFrom::Start(start)).await.map_err(|_| ApiError::Internal)?;
                (
                    StatusCode::PARTIAL_CONTENT,
                    Some(format!("bytes {start}-{end}/{size}")),
                    Some(len),
                )
            }
            Ok(None) => (StatusCode::OK, None, None),
            Err(_) => return Ok(range_not_satisfiable_response(size)),
        }
    } else {
        (StatusCode::OK, None, None)
    };

    let body: Body = match stream_len {
        Some(len) => {
            let stream = ReaderStream::new(file.take(len));
            Body::from_stream(stream)
        }
        None => {
            let stream = ReaderStream::new(file);
            Body::from_stream(stream)
        }
    };

    let mut resp = Response::builder()
        .status(status)
        .header(ACCEPT_RANGES, "bytes")
        .header(CONTENT_TYPE, "application/octet-stream")
        .header(ETAG, etag)
        .body(body)
        .map_err(|_| ApiError::Internal)?
        .into_response();

    if let Some(content_range) = content_range {
        resp.headers_mut()
            .insert(CONTENT_RANGE, HeaderValue::from_str(&content_range).map_err(|_| ApiError::Internal)?);
    }

    if let Some(len) = stream_len {
        resp.headers_mut()
            .insert(CONTENT_LENGTH, HeaderValue::from(len));
    } else {
        resp.headers_mut()
            .insert(CONTENT_LENGTH, HeaderValue::from(size));
    }

    Ok(resp)
}

use axum::http::HeaderName;

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

fn compute_etag(metadata: &std::fs::Metadata) -> HeaderValue {
    let size = metadata.len();
    let modified = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);

    let tag = format!("W/\"{size}-{modified}\"");
    HeaderValue::from_str(&tag).unwrap_or_else(|_| HeaderValue::from_static("W/\"0-0\""))
}

#[derive(Debug)]
enum RangeParseError {
    Invalid,
    Unsatisfiable,
}

fn parse_single_range(header_value: &str, size: u64) -> Result<Option<(u64, u64)>, RangeParseError> {
    let header_value = header_value.trim();
    if header_value.is_empty() {
        return Ok(None);
    }

    let Some(spec) = header_value.strip_prefix("bytes=") else {
        return Err(RangeParseError::Invalid);
    };

    if spec.contains(',') {
        return Err(RangeParseError::Invalid);
    }

    let (start_s, end_s) = spec
        .split_once('-')
        .ok_or(RangeParseError::Invalid)?;

    if size == 0 {
        return Err(RangeParseError::Unsatisfiable);
    }

    let last = size - 1;

    let (start, end) = if start_s.is_empty() {
        // suffix-byte-range-spec: "-<length>"
        let suffix_len: u64 = end_s.parse().map_err(|_| RangeParseError::Invalid)?;
        if suffix_len == 0 {
            return Err(RangeParseError::Unsatisfiable);
        }
        if suffix_len >= size {
            (0, last)
        } else {
            (size - suffix_len, last)
        }
    } else {
        let start: u64 = start_s.parse().map_err(|_| RangeParseError::Invalid)?;
        if start >= size {
            return Err(RangeParseError::Unsatisfiable);
        }

        if end_s.is_empty() {
            (start, last)
        } else {
            let mut end: u64 = end_s.parse().map_err(|_| RangeParseError::Invalid)?;
            if end < start {
                return Err(RangeParseError::Invalid);
            }
            if end > last {
                end = last;
            }
            (start, end)
        }
    };

    Ok(Some((start, end)))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use axum::http::Request;
    use http::header::ACCESS_CONTROL_REQUEST_HEADERS;
    use http::header::ACCESS_CONTROL_REQUEST_METHOD;
    use http::Method;
    use tower::ServiceExt;

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
        }
    }

    async fn write_file(path: &Path, data: &[u8]) {
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(path, data).await.unwrap();
    }

    #[tokio::test]
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
                .get(CONTENT_RANGE)
                .unwrap()
                .to_str()
                .unwrap(),
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

    #[tokio::test]
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
            resp.headers()
                .get(CONTENT_RANGE)
                .unwrap()
                .to_str()
                .unwrap(),
            "bytes */6"
        );
    }

    #[tokio::test]
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

    #[tokio::test]
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

    #[tokio::test]
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

    #[tokio::test]
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
}
