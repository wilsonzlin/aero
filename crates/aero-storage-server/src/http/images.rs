use crate::{
    cors::CorsConfig,
    http::{
        cache,
        range::{
            parse_range_header, resolve_range, ByteRange, RangeOptions, RangeParseError,
            RangeResolveError,
        },
    },
    metrics::Metrics,
    store::{ImageStore, StoreError},
};

use axum::{
    body::Body,
    extract::{Path, State},
    http::{
        header::{self, HeaderName, HeaderValue},
        HeaderMap, StatusCode,
    },
    response::Response,
    routing::get,
    Router,
};
use http_body::{Body as HttpBody, Frame, SizeHint};
use futures::Stream;
use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::io::ReaderStream;
use tracing::Instrument;

const DEFAULT_MAX_TOTAL_BYTES: u64 = 8 * 1024 * 1024;
const DEFAULT_PUBLIC_MAX_AGE: Duration = Duration::from_secs(60 * 60);

#[derive(Clone)]
pub struct ImagesState {
    store: Arc<dyn ImageStore>,
    metrics: Arc<Metrics>,
    range_options: RangeOptions,
    require_range: bool,
    cors: CorsConfig,
    cross_origin_resource_policy: HeaderValue,
    public_cache_max_age: Duration,
    bytes_request_semaphore: Option<Arc<Semaphore>>,
}

impl ImagesState {
    pub fn new(store: Arc<dyn ImageStore>, metrics: Arc<Metrics>) -> Self {
        Self {
            store,
            metrics,
            range_options: RangeOptions {
                max_total_bytes: DEFAULT_MAX_TOTAL_BYTES,
            },
            require_range: false,
            cors: CorsConfig::default(),
            cross_origin_resource_policy: HeaderValue::from_static("same-site"),
            public_cache_max_age: DEFAULT_PUBLIC_MAX_AGE,
            bytes_request_semaphore: Some(Arc::new(Semaphore::new(
                crate::DEFAULT_MAX_CONCURRENT_BYTES_REQUESTS,
            ))),
        }
    }

    pub fn with_range_options(mut self, range_options: RangeOptions) -> Self {
        self.range_options = range_options;
        self
    }

    /// When enabled, `GET` requests without a `Range` header will be rejected with
    /// `416 Range Not Satisfiable` instead of returning the full object body.
    pub fn with_require_range(mut self, require_range: bool) -> Self {
        self.require_range = require_range;
        self
    }

    pub fn with_cors(mut self, cors: CorsConfig) -> Self {
        self.cors = cors;
        self
    }

    pub fn with_cors_allow_origin(mut self, cors_allow_origin: HeaderValue) -> Self {
        self.cors = self.cors.with_allow_origin(cors_allow_origin);
        self
    }

    pub fn with_cors_allow_credentials(mut self, cors_allow_credentials: bool) -> Self {
        self.cors = self.cors.with_allow_credentials(cors_allow_credentials);
        self
    }

    pub fn with_cors_allowed_origins<I, S>(mut self, origins: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.cors = self.cors.with_allowed_origins(origins);
        self
    }

    pub fn with_cors_preflight_max_age(mut self, max_age: Duration) -> Self {
        self.cors = self.cors.with_preflight_max_age(max_age);
        self
    }

    pub fn with_cross_origin_resource_policy(
        mut self,
        cross_origin_resource_policy: HeaderValue,
    ) -> Self {
        self.cross_origin_resource_policy = cross_origin_resource_policy;
        self
    }

    /// Set the max-age used for publicly cacheable image bytes responses.
    pub fn with_public_cache_max_age(mut self, max_age: Duration) -> Self {
        self.public_cache_max_age = max_age;
        self
    }

    /// Set the maximum number of concurrent requests allowed to the image bytes endpoints.
    ///
    /// Use `0` to disable limiting (unlimited).
    pub fn with_max_concurrent_bytes_requests(mut self, max: usize) -> Self {
        self.bytes_request_semaphore = if max == 0 {
            None
        } else {
            Some(Arc::new(Semaphore::new(max)))
        };
        self
    }

    pub(crate) fn metrics(&self) -> &Metrics {
        self.metrics.as_ref()
    }
}

pub fn router() -> Router<ImagesState> {
    Router::<ImagesState>::new()
        .route(
            "/v1/images/:image_id/data",
            get(get_image).head(head_image).options(options_image),
        )
        .route(
            "/v1/images/:image_id",
            get(get_image).head(head_image).options(options_image),
        )
}

pub fn router_with_state(state: ImagesState) -> Router {
    router().with_state(state)
}

pub async fn get_image(
    Path(image_id): Path<String>,
    State(state): State<ImagesState>,
    headers: HeaderMap,
) -> Response {
    if crate::store::validate_image_id(&image_id).is_ok() {
        crate::http::observability::record_image_id(&image_id);
    } else {
        // Avoid recording attacker-controlled invalid values in the span.
        tracing::Span::current().record("image_id", tracing::field::display("<invalid>"));
        return response_with_status(StatusCode::NOT_FOUND, &state, &headers);
    }
    serve_image(image_id, state, headers, true).await
}

pub async fn head_image(
    Path(image_id): Path<String>,
    State(state): State<ImagesState>,
    headers: HeaderMap,
) -> Response {
    if crate::store::validate_image_id(&image_id).is_ok() {
        crate::http::observability::record_image_id(&image_id);
    } else {
        tracing::Span::current().record("image_id", tracing::field::display("<invalid>"));
        return response_with_status(StatusCode::NOT_FOUND, &state, &headers);
    }
    serve_image(image_id, state, headers, false).await
}

pub async fn options_image(State(state): State<ImagesState>, req_headers: HeaderMap) -> Response {
    let permit = match try_acquire_bytes_permit(&state, &req_headers) {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::NO_CONTENT;
    insert_cors_preflight_headers(response.headers_mut(), &state, &req_headers);
    attach_bytes_permit(response, permit)
}

async fn serve_image(
    image_id: String,
    state: ImagesState,
    req_headers: HeaderMap,
    want_body: bool,
) -> Response {
    let permit = match try_acquire_bytes_permit(&state, &req_headers) {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    let response = (async move {
        let image = match state.store.get_image(&image_id).await {
            Ok(image) => image,
            Err(StoreError::NotFound) | Err(StoreError::InvalidImageId { .. }) => {
                return response_with_status(StatusCode::NOT_FOUND, &state, &req_headers);
            }
            Err(StoreError::Manifest(err)) => {
                state.metrics.inc_store_error("manifest");
                tracing::error!(error = %err, "store manifest error");
                return response_with_status(StatusCode::INTERNAL_SERVER_ERROR, &state, &req_headers);
            }
            Err(StoreError::Io(err)) => {
                state.metrics.inc_store_error("meta");
                tracing::error!(error = %err, "store get_image failed");
                return response_with_status(StatusCode::INTERNAL_SERVER_ERROR, &state, &req_headers);
            }
            Err(StoreError::InvalidRange { .. }) => {
                // `get_image` doesn't currently produce this, but keep mapping defensive.
                state.metrics.inc_store_error("meta");
                return response_with_status(StatusCode::INTERNAL_SERVER_ERROR, &state, &req_headers);
            }
        };

        let image_public = image.public;
        let meta = image.meta;

        let len = meta.size;
        let cache_control = data_cache_control_value(&state, &req_headers, image_public);

        // Conditional requests (`If-None-Match` / `If-Modified-Since`) are evaluated against the
        // ETag we would send on success. Some store implementations may not provide an ETag; in that
        // case fall back to our deterministic weak ETag so `If-None-Match: *` and revalidation still
        // work.
        let fallback_etag = meta.etag.is_none().then(|| cache::etag_or_fallback(&meta));
        let current_etag = meta.etag.as_deref().or(fallback_etag.as_deref());

        // Conditional requests: if the client has a matching validator, we can return `304` and
        // avoid streaming bytes (RFC 9110).
        if cache::is_not_modified(&req_headers, current_etag, meta.last_modified) {
            return not_modified_response(&state, &req_headers, &meta, cache_control);
        }

        let range_header = req_headers.get(header::RANGE).and_then(|v| v.to_str().ok());
        if range_header.is_none() && state.require_range && want_body {
            // Range-only mode: avoid accidental full-object downloads when a browser/crawler hits the
            // bytes endpoint without `Range`.
            state.metrics.inc_range_request_invalid();
            return range_not_satisfiable(&state, &req_headers, len);
        }
        if let Some(range_header) = range_header {
            // RFC 9110 If-Range support: if validator doesn't match, ignore Range and return 200.
            if !cache::if_range_allows_range(&req_headers, meta.etag.as_deref(), meta.last_modified)
            {
                return full_response(&state, &req_headers, &image_id, meta, want_body, cache_control)
                    .await;
            }

            let specs = match parse_range_header(range_header) {
                Ok(Some(v)) => v,
                Ok(None) => {
                    return full_response(
                        &state,
                        &req_headers,
                        &image_id,
                        meta,
                        want_body,
                        cache_control,
                    )
                    .await
                }
                Err(
                    RangeParseError::HeaderTooLarge { .. } | RangeParseError::TooManyRanges { .. },
                ) => {
                    state.metrics.inc_range_request_invalid();
                    return response_with_status(StatusCode::PAYLOAD_TOO_LARGE, &state, &req_headers);
                }
                Err(_) => {
                    // For syntactically invalid ranges, follow our public contract and return `416`.
                    // (See `docs/16-disk-image-streaming-auth.md`.)
                    state.metrics.inc_range_request_invalid();
                    return range_not_satisfiable(&state, &req_headers, len);
                }
            };

            let range = match resolve_range(&specs, len, state.range_options) {
                Ok(r) => r,
                // Abuse guard: we cap the maximum single-range response size to avoid clients forcing
                // huge reads (amplification / resource exhaustion).
                Err(RangeResolveError::TooManyBytes) => {
                    state.metrics.inc_range_request_invalid();
                    return response_with_status(StatusCode::PAYLOAD_TOO_LARGE, &state, &req_headers);
                }
                // Disk streaming only supports a single range per request.
                Err(
                    RangeResolveError::MultiRangeNotSupported | RangeResolveError::Unsatisfiable,
                ) => {
                    state.metrics.inc_range_request_invalid();
                    return range_not_satisfiable(&state, &req_headers, len);
                }
            };

            state.metrics.inc_range_request_valid();

            return single_range_response(
                &state,
                &req_headers,
                &image_id,
                meta,
                range,
                want_body,
                cache_control,
            )
            .await;
        }

        full_response(&state, &req_headers, &image_id, meta, want_body, cache_control).await
    })
    .await;

    attach_bytes_permit(response, permit)
}

fn try_acquire_bytes_permit(
    state: &ImagesState,
    req_headers: &HeaderMap,
) -> Result<Option<OwnedSemaphorePermit>, Response> {
    let Some(sem) = state.bytes_request_semaphore.as_ref() else {
        return Ok(None);
    };

    match sem.clone().try_acquire_owned() {
        Ok(permit) => Ok(Some(permit)),
        Err(_) => Err(response_with_status(
            StatusCode::TOO_MANY_REQUESTS,
            state,
            req_headers,
        )),
    }
}

fn attach_bytes_permit(mut response: Response, permit: Option<OwnedSemaphorePermit>) -> Response {
    let Some(permit) = permit else {
        return response;
    };

    let body = std::mem::replace(response.body_mut(), Body::empty());
    *response.body_mut() = Body::new(PermitBody::new(body, permit));
    response
}

struct PermitBody {
    inner: Pin<Box<Body>>,
    _permit: OwnedSemaphorePermit,
}

impl PermitBody {
    fn new(inner: Body, permit: OwnedSemaphorePermit) -> Self {
        Self {
            inner: Box::pin(inner),
            _permit: permit,
        }
    }
}

impl HttpBody for PermitBody {
    type Data = <Body as HttpBody>::Data;
    type Error = <Body as HttpBody>::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        this.inner.as_mut().poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.as_ref().get_ref().is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.as_ref().get_ref().size_hint()
    }
}

async fn full_response(
    state: &ImagesState,
    req_headers: &HeaderMap,
    image_id: &str,
    meta: crate::store::ImageMeta,
    want_body: bool,
    cache_control: HeaderValue,
) -> Response {
    if want_body {
        state
            .metrics
            .observe_image_bytes_served(image_id, meta.size);
    }

    let mut response = Response::new(if want_body {
        let image_id_for_span =
            crate::http::observability::truncate_for_span(image_id, crate::store::MAX_IMAGE_ID_LEN);
        let span = tracing::info_span!(
            "store_read",
            image_id = %image_id_for_span,
            start = 0_u64,
            len = meta.size
        );
        match state
            .store
            .open_range(image_id, 0, meta.size)
            .instrument(span.clone())
            .await
        {
            Ok(reader) => {
                let stream = InstrumentedStream::new(ReaderStream::new(reader), span);
                Body::from_stream(stream)
            }
            Err(err) => return response_from_store_error(state, req_headers, err, meta.size),
        }
    } else {
        Body::empty()
    });

    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    insert_cors_headers(headers, state, req_headers);
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(meta.content_type),
    );
    headers.insert(
        header::CONTENT_ENCODING,
        HeaderValue::from_static("identity"),
    );
    headers.insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&meta.size.to_string()).unwrap(),
    );
    insert_data_cache_headers(headers, &meta, cache_control);
    response
}

async fn single_range_response(
    state: &ImagesState,
    req_headers: &HeaderMap,
    image_id: &str,
    meta: crate::store::ImageMeta,
    range: ByteRange,
    want_body: bool,
    cache_control: HeaderValue,
) -> Response {
    let range_len = range.len();
    if want_body {
        state
            .metrics
            .observe_image_bytes_served(image_id, range_len);
    }

    let mut response = Response::new(if want_body {
        let image_id_for_span =
            crate::http::observability::truncate_for_span(image_id, crate::store::MAX_IMAGE_ID_LEN);
        let span = tracing::info_span!(
            "store_read",
            image_id = %image_id_for_span,
            start = range.start,
            len = range_len,
        );
        match state
            .store
            .open_range(image_id, range.start, range_len)
            .instrument(span.clone())
            .await
        {
            Ok(reader) => {
                let stream = InstrumentedStream::new(ReaderStream::new(reader), span);
                Body::from_stream(stream)
            }
            Err(err) => return response_from_store_error(state, req_headers, err, meta.size),
        }
    } else {
        Body::empty()
    });

    *response.status_mut() = StatusCode::PARTIAL_CONTENT;
    let headers = response.headers_mut();
    insert_cors_headers(headers, state, req_headers);
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(meta.content_type),
    );
    headers.insert(
        header::CONTENT_ENCODING,
        HeaderValue::from_static("identity"),
    );
    headers.insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_RANGE,
        HeaderValue::from_str(&format!(
            "bytes {}-{}/{}",
            range.start, range.end, meta.size
        ))
        .unwrap(),
    );
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&range_len.to_string()).unwrap(),
    );
    insert_data_cache_headers(headers, &meta, cache_control);
    response
}

fn range_not_satisfiable(state: &ImagesState, req_headers: &HeaderMap, len: u64) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
    let headers = response.headers_mut();
    insert_cors_headers(headers, state, req_headers);
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, no-transform"),
    );
    headers.insert(
        header::CONTENT_RANGE,
        HeaderValue::from_str(&format!("bytes */{len}")).unwrap(),
    );
    response
}

fn response_from_store_error(
    state: &ImagesState,
    req_headers: &HeaderMap,
    err: StoreError,
    len: u64,
) -> Response {
    match err {
        StoreError::NotFound | StoreError::InvalidImageId { .. } => {
            response_with_status(StatusCode::NOT_FOUND, state, req_headers)
        }
        StoreError::InvalidRange { .. } => range_not_satisfiable(state, req_headers, len),
        StoreError::Manifest(err) => {
            state.metrics.inc_store_error("manifest");
            tracing::error!(error = %err, "store manifest error");
            response_with_status(StatusCode::INTERNAL_SERVER_ERROR, state, req_headers)
        }
        StoreError::Io(err) => {
            state.metrics.inc_store_error("open_range");
            tracing::error!(error = %err, "store open_range failed");
            response_with_status(StatusCode::INTERNAL_SERVER_ERROR, state, req_headers)
        }
    }
}

fn response_with_status(status: StatusCode, state: &ImagesState, req_headers: &HeaderMap) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = status;
    let headers = response.headers_mut();
    insert_cors_headers(headers, state, req_headers);
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, no-transform"),
    );
    response
}

pub(crate) fn insert_cors_headers(
    headers: &mut HeaderMap,
    state: &ImagesState,
    req_headers: &HeaderMap,
) {
    state.cors.insert_cors_headers(
        headers,
        req_headers,
        Some(HeaderValue::from_static(
            "ETag, Last-Modified, Cache-Control, Content-Range, Accept-Ranges, Content-Length",
        )),
    );
    headers.insert(
        HeaderName::from_static("cross-origin-resource-policy"),
        state.cross_origin_resource_policy.clone(),
    );
}

pub(crate) fn insert_cors_preflight_headers(
    headers: &mut HeaderMap,
    state: &ImagesState,
    req_headers: &HeaderMap,
) {
    state.cors.insert_cors_preflight_headers(
        headers,
        req_headers,
        HeaderValue::from_static("GET, HEAD, OPTIONS"),
        HeaderValue::from_static(
            "Range, If-Range, If-None-Match, If-Modified-Since, Authorization, Content-Type",
        ),
    );
    headers.insert(
        HeaderName::from_static("cross-origin-resource-policy"),
        state.cross_origin_resource_policy.clone(),
    );
}

fn insert_data_cache_headers(
    headers: &mut HeaderMap,
    meta: &crate::store::ImageMeta,
    cache_control: HeaderValue,
) {
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(header::CACHE_CONTROL, cache_control);
    headers.insert(
        header::ETAG,
        HeaderValue::from_str(&cache::etag_or_fallback(meta)).unwrap(),
    );
    if let Some(last_modified) = cache::last_modified_header_value(meta.last_modified) {
        headers.insert(header::LAST_MODIFIED, last_modified);
    }
}

fn data_cache_control_value(
    state: &ImagesState,
    req_headers: &HeaderMap,
    image_public: bool,
) -> HeaderValue {
    // Respect the per-image manifest cacheability. Even if a request is made without credentials,
    // a manifest-private image must not become publicly cacheable.
    //
    // This aligns the bytes endpoints with the safety expectations documented in
    // `docs/16-disk-image-streaming-auth.md`.
    if !image_public {
        return HeaderValue::from_static("private, no-store, no-transform");
    }

    // Treat any request that includes credentials (Authorization header or cookies) as private.
    // This is a conservative default: public disk bytes responses should be cacheable, but
    // authenticated responses must not be cached by shared intermediaries.
    if req_headers.contains_key(header::AUTHORIZATION) || req_headers.contains_key(header::COOKIE) {
        HeaderValue::from_static("private, no-store, no-transform")
    } else {
        let secs = state.public_cache_max_age.as_secs();
        HeaderValue::from_str(&format!("public, max-age={secs}, no-transform")).unwrap()
    }
}

fn not_modified_response(
    state: &ImagesState,
    req_headers: &HeaderMap,
    meta: &crate::store::ImageMeta,
    cache_control: HeaderValue,
) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::NOT_MODIFIED;
    let headers = response.headers_mut();
    insert_cors_headers(headers, state, req_headers);
    insert_data_cache_headers(headers, meta, cache_control);
    response
}

struct InstrumentedStream<S> {
    inner: Pin<Box<S>>,
    span: tracing::Span,
}

impl<S> InstrumentedStream<S> {
    fn new(inner: S, span: tracing::Span) -> Self {
        Self {
            inner: Box::pin(inner),
            span,
        }
    }
}

impl<S> Stream for InstrumentedStream<S>
where
    S: Stream,
{
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let _guard = this.span.enter();
        this.inner.as_mut().poll_next(cx)
    }
}
