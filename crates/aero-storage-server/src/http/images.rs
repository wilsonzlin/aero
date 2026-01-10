use crate::{
    http::range::{parse_range_header, resolve_ranges, ByteRange, RangeOptions, RangeResolveError},
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
use std::{io, pin::Pin, sync::Arc};
use tokio_util::io::ReaderStream;

const DEFAULT_MAX_RANGES: usize = 16;
const DEFAULT_MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone)]
pub struct ImagesState {
    store: Arc<dyn ImageStore>,
    range_options: RangeOptions,
    cors_allow_origin: HeaderValue,
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
}

pub fn router(store: Arc<dyn ImageStore>) -> Router {
    router_with_state(ImagesState::new(store))
}

pub fn router_with_state(state: ImagesState) -> Router {
    Router::new()
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

    let range_header = req_headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    if let Some(range_header) = range_header {
        let specs = match parse_range_header(range_header) {
            Ok(Some(v)) => v,
            Ok(None) => return full_response(&state, &image_id, meta, want_body).await,
            Err(_) => return range_not_satisfiable(&state, len),
        };

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
            return single_range_response(&state, &image_id, meta, ranges[0], want_body).await;
        }
        return multipart_range_response(&state, &image_id, meta, ranges, want_body).await;
    }

    full_response(&state, &image_id, meta, want_body).await
}

async fn full_response(
    state: &ImagesState,
    image_id: &str,
    meta: crate::store::ImageMeta,
    want_body: bool,
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
    insert_common_headers(headers, state);
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(meta.content_type),
    );
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&meta.size.to_string()).unwrap(),
    );
    if let Some(etag) = meta.etag {
        headers.insert(header::ETAG, HeaderValue::from_str(&etag).unwrap());
    }
    response
}

async fn single_range_response(
    state: &ImagesState,
    image_id: &str,
    meta: crate::store::ImageMeta,
    range: ByteRange,
    want_body: bool,
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
    insert_common_headers(headers, state);
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
    if let Some(etag) = meta.etag {
        headers.insert(header::ETAG, HeaderValue::from_str(&etag).unwrap());
    }
    response
}

async fn multipart_range_response(
    state: &ImagesState,
    image_id: &str,
    meta: crate::store::ImageMeta,
    ranges: Vec<ByteRange>,
    want_body: bool,
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

                let reader = store.open_range(&image_id, range.start, range.len()).await
                    .map_err(|err| io::Error::new(io::ErrorKind::Other, err))?;

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
    insert_common_headers(headers, state);
    headers.insert(header::CONTENT_TYPE, content_type);
    if let Some(etag) = meta.etag {
        headers.insert(header::ETAG, HeaderValue::from_str(&etag).unwrap());
    }
    response
}

fn range_not_satisfiable(state: &ImagesState, len: u64) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
    let headers = response.headers_mut();
    insert_common_headers(headers, state);
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
    insert_common_headers(response.headers_mut(), state);
    response
}

fn insert_common_headers(headers: &mut HeaderMap, state: &ImagesState) {
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-transform"),
    );

    headers.insert(
        HeaderName::from_static("access-control-allow-origin"),
        state.cors_allow_origin.clone(),
    );
    headers.insert(
        HeaderName::from_static("access-control-expose-headers"),
        HeaderValue::from_static("ETag, Content-Range, Accept-Ranges, Content-Length"),
    );
}

fn insert_cors_preflight_headers(headers: &mut HeaderMap, state: &ImagesState) {
    headers.insert(
        HeaderName::from_static("access-control-allow-origin"),
        state.cors_allow_origin.clone(),
    );
    headers.insert(
        HeaderName::from_static("access-control-allow-methods"),
        HeaderValue::from_static("GET, HEAD, OPTIONS"),
    );
    headers.insert(
        HeaderName::from_static("access-control-allow-headers"),
        HeaderValue::from_static("Range, If-Range, Content-Type"),
    );
    headers.insert(
        HeaderName::from_static("access-control-max-age"),
        HeaderValue::from_static("86400"),
    );
}
