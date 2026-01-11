use crate::{
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
use futures::Stream;
use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio_util::io::ReaderStream;
use tracing::Instrument;

const DEFAULT_MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
const DEFAULT_PUBLIC_MAX_AGE: Duration = Duration::from_secs(60 * 60);

#[derive(Clone)]
pub struct ImagesState {
    store: Arc<dyn ImageStore>,
    metrics: Arc<Metrics>,
    range_options: RangeOptions,
    cors_allow_origin: HeaderValue,
    public_cache_max_age: Duration,
}

impl ImagesState {
    pub fn new(store: Arc<dyn ImageStore>, metrics: Arc<Metrics>) -> Self {
        Self {
            store,
            metrics,
            range_options: RangeOptions {
                max_total_bytes: DEFAULT_MAX_TOTAL_BYTES,
            },
            cors_allow_origin: HeaderValue::from_static("*"),
            public_cache_max_age: DEFAULT_PUBLIC_MAX_AGE,
        }
    }

    pub fn with_range_options(mut self, range_options: RangeOptions) -> Self {
        self.range_options = range_options;
        self
    }

    pub fn with_cors_allow_origin(mut self, cors_allow_origin: HeaderValue) -> Self {
        self.cors_allow_origin = cors_allow_origin;
        self
    }

    /// Set the max-age used for publicly cacheable image bytes responses.
    pub fn with_public_cache_max_age(mut self, max_age: Duration) -> Self {
        self.public_cache_max_age = max_age;
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
    tracing::Span::current().record("image_id", &tracing::field::display(&image_id));
    serve_image(image_id, state, headers, true).await
}

pub async fn head_image(
    Path(image_id): Path<String>,
    State(state): State<ImagesState>,
    headers: HeaderMap,
) -> Response {
    tracing::Span::current().record("image_id", &tracing::field::display(&image_id));
    serve_image(image_id, state, headers, false).await
}

pub async fn options_image(State(state): State<ImagesState>) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::NO_CONTENT;
    insert_cors_preflight_headers(response.headers_mut(), &state);
    response
}

async fn serve_image(
    image_id: String,
    state: ImagesState,
    req_headers: HeaderMap,
    want_body: bool,
) -> Response {
    let meta = match state.store.get_meta(&image_id).await {
        Ok(meta) => meta,
        Err(StoreError::NotFound) | Err(StoreError::InvalidImageId { .. }) => {
            return response_with_status(StatusCode::NOT_FOUND, &state);
        }
        Err(StoreError::Manifest(err)) => {
            state.metrics.inc_store_error("manifest");
            tracing::error!(error = %err, "store manifest error");
            return response_with_status(StatusCode::INTERNAL_SERVER_ERROR, &state);
        }
        Err(StoreError::Io(err)) => {
            state.metrics.inc_store_error("meta");
            tracing::error!(error = %err, "store get_meta failed");
            return response_with_status(StatusCode::INTERNAL_SERVER_ERROR, &state);
        }
        Err(StoreError::InvalidRange { .. }) => {
            // `get_meta` doesn't currently produce this, but keep mapping defensive.
            state.metrics.inc_store_error("meta");
            return response_with_status(StatusCode::INTERNAL_SERVER_ERROR, &state);
        }
    };

    let len = meta.size;
    let cache_control = data_cache_control_value(&state, &req_headers);

    // Conditional requests: only required for HEAD, but safe for GET too (when present).
    if !want_body && cache::is_not_modified(&req_headers, meta.etag.as_deref(), meta.last_modified)
    {
        return not_modified_response(&state, &meta, cache_control);
    }

    let range_header = req_headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    if let Some(range_header) = range_header {
        // RFC 9110 If-Range support: if validator doesn't match, ignore Range and return 200.
        if !cache::if_range_allows_range(&req_headers, meta.etag.as_deref(), meta.last_modified) {
            return full_response(&state, &image_id, meta, want_body, cache_control).await;
        }

        let specs = match parse_range_header(range_header) {
            Ok(Some(v)) => v,
            Ok(None) => {
                return full_response(&state, &image_id, meta, want_body, cache_control).await
            }
            Err(RangeParseError::HeaderTooLarge { .. } | RangeParseError::TooManyRanges { .. }) => {
                state.metrics.inc_range_request_invalid();
                return response_with_status(StatusCode::PAYLOAD_TOO_LARGE, &state);
            }
            Err(_) => {
                // For syntactically invalid ranges, follow our public contract and return `416`.
                // (See `docs/16-disk-image-streaming-auth.md`.)
                state.metrics.inc_range_request_invalid();
                return range_not_satisfiable(&state, len);
            }
        };

        let range = match resolve_range(&specs, len, state.range_options) {
            Ok(r) => r,
            // Abuse guard: we cap the maximum single-range response size to avoid clients forcing
            // huge reads (amplification / resource exhaustion).
            Err(RangeResolveError::TooManyBytes) => {
                state.metrics.inc_range_request_invalid();
                return response_with_status(StatusCode::PAYLOAD_TOO_LARGE, &state);
            }
            // Disk streaming only supports a single range per request.
            Err(RangeResolveError::MultiRangeNotSupported | RangeResolveError::Unsatisfiable) => {
                state.metrics.inc_range_request_invalid();
                return range_not_satisfiable(&state, len);
            }
        };

        state.metrics.inc_range_request_valid();

        return single_range_response(&state, &image_id, meta, range, want_body, cache_control)
            .await;
    }

    full_response(&state, &image_id, meta, want_body, cache_control).await
}

async fn full_response(
    state: &ImagesState,
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
        let span =
            tracing::info_span!("store_read", image_id = %image_id, start = 0_u64, len = meta.size);
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
            Err(err) => return response_from_store_error(state, err, meta.size),
        }
    } else {
        Body::empty()
    });

    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    insert_cors_headers(headers, state);
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(meta.content_type),
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
        let span = tracing::info_span!(
            "store_read",
            image_id = %image_id,
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
            Err(err) => return response_from_store_error(state, err, meta.size),
        }
    } else {
        Body::empty()
    });

    *response.status_mut() = StatusCode::PARTIAL_CONTENT;
    let headers = response.headers_mut();
    insert_cors_headers(headers, state);
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(meta.content_type),
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

fn range_not_satisfiable(state: &ImagesState, len: u64) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
    let headers = response.headers_mut();
    insert_cors_headers(headers, state);
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

fn response_from_store_error(state: &ImagesState, err: StoreError, len: u64) -> Response {
    match err {
        StoreError::NotFound | StoreError::InvalidImageId { .. } => {
            response_with_status(StatusCode::NOT_FOUND, state)
        }
        StoreError::InvalidRange { .. } => range_not_satisfiable(state, len),
        StoreError::Manifest(err) => {
            state.metrics.inc_store_error("manifest");
            tracing::error!(error = %err, "store manifest error");
            response_with_status(StatusCode::INTERNAL_SERVER_ERROR, state)
        }
        StoreError::Io(err) => {
            state.metrics.inc_store_error("open_range");
            tracing::error!(error = %err, "store open_range failed");
            response_with_status(StatusCode::INTERNAL_SERVER_ERROR, state)
        }
    }
}

fn response_with_status(status: StatusCode, state: &ImagesState) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = status;
    let headers = response.headers_mut();
    insert_cors_headers(headers, state);
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, no-transform"),
    );
    response
}

pub(crate) fn insert_cors_headers(headers: &mut HeaderMap, state: &ImagesState) {
    headers.insert(
        HeaderName::from_static("access-control-allow-origin"),
        state.cors_allow_origin.clone(),
    );
    // Be conservative: even when `Access-Control-Allow-Origin: *`, varying on Origin is safe and
    // avoids surprising cache poisoning if deployments change to an allowlist.
    headers.insert(header::VARY, HeaderValue::from_static("Origin"));
    headers.insert(
        HeaderName::from_static("access-control-expose-headers"),
        HeaderValue::from_static(
            "ETag, Last-Modified, Cache-Control, Content-Range, Accept-Ranges, Content-Length",
        ),
    );
}

pub(crate) fn insert_cors_preflight_headers(headers: &mut HeaderMap, state: &ImagesState) {
    headers.insert(
        HeaderName::from_static("access-control-allow-origin"),
        state.cors_allow_origin.clone(),
    );
    headers.insert(
        header::VARY,
        HeaderValue::from_static(
            "Origin, Access-Control-Request-Method, Access-Control-Request-Headers",
        ),
    );
    headers.insert(
        HeaderName::from_static("access-control-allow-methods"),
        HeaderValue::from_static("GET, HEAD, OPTIONS"),
    );
    headers.insert(
        HeaderName::from_static("access-control-allow-headers"),
        HeaderValue::from_static(
            "Range, If-Range, If-None-Match, If-Modified-Since, Authorization, Content-Type",
        ),
    );
    headers.insert(
        HeaderName::from_static("access-control-max-age"),
        HeaderValue::from_static("86400"),
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

fn data_cache_control_value(state: &ImagesState, req_headers: &HeaderMap) -> HeaderValue {
    if req_headers.contains_key(header::AUTHORIZATION) {
        HeaderValue::from_static("private, no-store, no-transform")
    } else {
        let secs = state.public_cache_max_age.as_secs();
        HeaderValue::from_str(&format!("public, max-age={secs}, no-transform")).unwrap()
    }
}

fn not_modified_response(
    state: &ImagesState,
    meta: &crate::store::ImageMeta,
    cache_control: HeaderValue,
) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::NOT_MODIFIED;
    let headers = response.headers_mut();
    insert_cors_headers(headers, state);
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
