use crate::{
    http::{
        cache,
        range::{parse_range_header, resolve_ranges, ByteRange, RangeOptions, RangeResolveError},
    },
    store::{ImageStore, StoreError},
};
use async_stream::try_stream;
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
use bytes::Bytes;
use futures::StreamExt;
use rand::{distributions::Alphanumeric, Rng};
use std::{io, pin::Pin, sync::Arc, time::Duration};
use tokio_util::io::ReaderStream;

const DEFAULT_MAX_RANGES: usize = 16;
const DEFAULT_MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
const DEFAULT_PUBLIC_MAX_AGE: Duration = Duration::from_secs(60 * 60);

#[derive(Clone)]
pub struct ImagesState {
    store: Arc<dyn ImageStore>,
    range_options: RangeOptions,
    cors_allow_origin: HeaderValue,
    public_cache_max_age: Duration,
}

impl ImagesState {
    pub fn new(store: Arc<dyn ImageStore>) -> Self {
        Self {
            store,
            range_options: RangeOptions {
                max_ranges: DEFAULT_MAX_RANGES,
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
}

pub fn router(store: Arc<dyn ImageStore>) -> Router {
    router_with_state(ImagesState::new(store))
}

pub fn router_with_state(state: ImagesState) -> Router {
    Router::new()
        .route(
            "/v1/images/:image_id/data",
            get(get_image).head(head_image).options(options_image),
        )
        .route(
            "/v1/images/:image_id",
            get(get_image).head(head_image).options(options_image),
        )
        .with_state(state)
}

pub async fn get_image(
    Path(image_id): Path<String>,
    State(state): State<ImagesState>,
    headers: HeaderMap,
) -> Response {
    serve_image(image_id, state, headers, true).await
}

pub async fn head_image(
    Path(image_id): Path<String>,
    State(state): State<ImagesState>,
    headers: HeaderMap,
) -> Response {
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
            return response_with_status(StatusCode::NOT_FOUND, &state)
        }
        Err(_) => return response_with_status(StatusCode::INTERNAL_SERVER_ERROR, &state),
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
        let specs = match parse_range_header(range_header) {
            Ok(Some(v)) => v,
            Ok(None) => {
                return full_response(&state, &image_id, meta, want_body, cache_control).await
            }
            Err(_) => return range_not_satisfiable(&state, len),
        };

        // RFC 9110 If-Range support: if validator doesn't match, ignore Range and return 200.
        if !cache::if_range_allows_range(&req_headers, meta.etag.as_deref(), meta.last_modified) {
            return full_response(&state, &image_id, meta, want_body, cache_control).await;
        }

        let ranges = match resolve_ranges(&specs, len, state.range_options) {
            Ok(r) => r,
            // Abuse guard: large multi-range requests can be used for amplification. We return
            // 413 rather than attempting to serve it.
            Err(RangeResolveError::TooManyRanges | RangeResolveError::TooManyBytes) => {
                return response_with_status(StatusCode::PAYLOAD_TOO_LARGE, &state)
            }
            Err(RangeResolveError::NoSatisfiableRanges) => {
                return range_not_satisfiable(&state, len)
            }
        };

        if ranges.len() == 1 {
            return single_range_response(
                &state,
                &image_id,
                meta,
                ranges[0],
                want_body,
                cache_control,
            )
            .await;
        }
        return multipart_range_response(
            &state,
            &image_id,
            meta,
            ranges,
            want_body,
            cache_control,
        )
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
    let mut response = Response::new(if want_body {
        match state.store.open_range(image_id, 0, meta.size).await {
            Ok(reader) => Body::from_stream(ReaderStream::new(reader)),
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
    let mut response = Response::new(if want_body {
        match state
            .store
            .open_range(image_id, range.start, range_len)
            .await
        {
            Ok(reader) => Body::from_stream(ReaderStream::new(reader)),
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

async fn multipart_range_response(
    state: &ImagesState,
    image_id: &str,
    meta: crate::store::ImageMeta,
    ranges: Vec<ByteRange>,
    want_body: bool,
    cache_control: HeaderValue,
) -> Response {
    let boundary: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect();

    let content_type =
        HeaderValue::from_str(&format!("multipart/byteranges; boundary={boundary}")).unwrap();

    let mut response = Response::new(if want_body {
        let store = Arc::clone(&state.store);
        let image_id = image_id.to_string();
        let boundary = boundary.clone();
        let content_type_part = meta.content_type;
        let total_size = meta.size;

        let stream = try_stream! {
            for range in ranges {
                let part_headers = format!(
                    "--{boundary}\r\nContent-Type: {content_type_part}\r\nContent-Range: bytes {start}-{end}/{total_size}\r\n\r\n",
                    start = range.start,
                    end = range.end,
                );
                yield Bytes::from(part_headers);

                let reader = store
                    .open_range(&image_id, range.start, range.len())
                    .await
                    .map_err(io::Error::other)?;

                let mut reader_stream = ReaderStream::new(reader);
                while let Some(chunk) = reader_stream.next().await {
                    yield chunk?;
                }
                yield Bytes::from_static(b"\r\n");
            }
            yield Bytes::from(format!("--{boundary}--\r\n"));
        };

        let stream: Pin<Box<dyn futures::Stream<Item = Result<Bytes, io::Error>> + Send>> =
            Box::pin(stream);

        Body::from_stream(stream)
    } else {
        Body::empty()
    });

    *response.status_mut() = StatusCode::PARTIAL_CONTENT;
    let headers = response.headers_mut();
    insert_cors_headers(headers, state);
    headers.insert(header::CONTENT_TYPE, content_type);
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
        StoreError::Manifest(_) => response_with_status(StatusCode::INTERNAL_SERVER_ERROR, state),
        StoreError::Io(_) => response_with_status(StatusCode::INTERNAL_SERVER_ERROR, state),
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
        HeaderValue::from_static("Origin, Access-Control-Request-Method, Access-Control-Request-Headers"),
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
