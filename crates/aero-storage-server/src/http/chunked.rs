use axum::{
    body::Body,
    extract::{Path, State},
    http::{
        header::{self, HeaderName, HeaderValue},
        HeaderMap, Request, StatusCode,
    },
    middleware::Next,
    response::Response,
    routing::get,
    Router,
};
use tokio_util::io::ReaderStream;
use tracing::Instrument;

use crate::{
    http::{cache, images},
    store::{ImageMeta, StoreError},
};

use super::images::ImagesState;

// Keep defaults aligned with `docs/18-chunked-disk-image-format.md`.
const PUBLIC_CACHE_CONTROL_CHUNKS: &str = "public, max-age=31536000, immutable, no-transform";
const PUBLIC_CACHE_CONTROL_MANIFEST: &str = "public, max-age=31536000, immutable";

// `00000000.bin` (zero-padded decimal chunk index, width=8) per docs.
const CHUNK_NAME_LEN: usize = 12;

pub fn router() -> Router<ImagesState> {
    Router::<ImagesState>::new()
        .route(
            "/v1/images/:image_id/chunked/manifest",
            get(get_manifest)
                .head(head_manifest)
                .options(options_manifest),
        )
        .route(
            "/v1/images/:image_id/chunked/manifest.json",
            get(get_manifest)
                .head(head_manifest)
                .options(options_manifest),
        )
        .route(
            "/v1/images/:image_id/chunked/:version/manifest",
            get(get_manifest_version)
                .head(head_manifest_version)
                .options(options_manifest),
        )
        .route(
            "/v1/images/:image_id/chunked/:version/manifest.json",
            get(get_manifest_version)
                .head(head_manifest_version)
                .options(options_manifest),
        )
        .route(
            "/v1/images/:image_id/chunked/chunks/:chunk_name",
            get(get_chunk).head(head_chunk).options(options_chunk),
        )
        .route(
            "/v1/images/:image_id/chunked/:version/chunks/:chunk_name",
            get(get_chunk_version)
                .head(head_chunk_version)
                .options(options_chunk),
        )
}

pub(crate) async fn chunk_name_path_len_guard(
    State(state): State<ImagesState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let path = req.uri().path();
    let Some(rest) = path.strip_prefix("/v1/images/") else {
        return next.run(req).await;
    };
    let mut parts = rest.split('/');
    // image_id segment (validated elsewhere).
    let _ = parts.next().unwrap_or("");
    if parts.next() != Some("chunked") {
        return next.run(req).await;
    }
    let Some(seg) = parts.next() else {
        return next.run(req).await;
    };

    // Unversioned chunk route: `/chunked/chunks/:chunk_name`.
    if seg == "chunks" {
        let raw_chunk = parts.next().unwrap_or("");
        if raw_chunk.len() > CHUNK_NAME_LEN * 3 {
            return response_with_status(StatusCode::NOT_FOUND, &state, req.headers());
        }
        return next.run(req).await;
    }

    // Versioned routes: `/chunked/:version/...`
    //
    // Guard the raw version segment length as well to avoid allocating attacker-controlled huge
    // values in `Path<String>` extraction.
    if seg.len() > crate::store::MAX_IMAGE_ID_LEN * 3 {
        return response_with_status(StatusCode::NOT_FOUND, &state, req.headers());
    }

    let Some(after_version) = parts.next() else {
        return next.run(req).await;
    };
    if after_version == "chunks" {
        let raw_chunk = parts.next().unwrap_or("");
        if raw_chunk.len() > CHUNK_NAME_LEN * 3 {
            return response_with_status(StatusCode::NOT_FOUND, &state, req.headers());
        }
    }

    next.run(req).await
}

pub async fn options_manifest(State(state): State<ImagesState>, req_headers: HeaderMap) -> Response {
    let permit = match images::try_acquire_bytes_permit(&state, &req_headers) {
        Ok(p) => p,
        Err(resp) => return *resp,
    };

    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::NO_CONTENT;
    images::insert_cors_preflight_headers(response.headers_mut(), &state, &req_headers);
    images::attach_bytes_permit(response, permit)
}

pub async fn options_chunk(State(state): State<ImagesState>, req_headers: HeaderMap) -> Response {
    // Chunk requests are plain `GET` in the normal case (no preflight), but OPTIONS is still
    // useful when clients send credentials or non-safelisted headers.
    options_manifest(State(state), req_headers).await
}

pub async fn get_manifest(
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
    serve_manifest(image_id, state, headers, true).await
}

pub async fn head_manifest(
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
    serve_manifest(image_id, state, headers, false).await
}

pub async fn get_manifest_version(
    Path((image_id, version)): Path<(String, String)>,
    State(state): State<ImagesState>,
    headers: HeaderMap,
) -> Response {
    if crate::store::validate_image_id(&image_id).is_ok()
        && crate::store::validate_image_id(&version).is_ok()
    {
        crate::http::observability::record_image_id(&image_id);
    } else {
        tracing::Span::current().record("image_id", tracing::field::display("<invalid>"));
        return response_with_status(StatusCode::NOT_FOUND, &state, &headers);
    }
    serve_manifest_version(image_id, version, state, headers, true).await
}

pub async fn head_manifest_version(
    Path((image_id, version)): Path<(String, String)>,
    State(state): State<ImagesState>,
    headers: HeaderMap,
) -> Response {
    if crate::store::validate_image_id(&image_id).is_ok()
        && crate::store::validate_image_id(&version).is_ok()
    {
        crate::http::observability::record_image_id(&image_id);
    } else {
        tracing::Span::current().record("image_id", tracing::field::display("<invalid>"));
        return response_with_status(StatusCode::NOT_FOUND, &state, &headers);
    }
    serve_manifest_version(image_id, version, state, headers, false).await
}

async fn serve_manifest(
    image_id: String,
    state: ImagesState,
    req_headers: HeaderMap,
    want_body: bool,
) -> Response {
    let permit = match images::try_acquire_bytes_permit(&state, &req_headers) {
        Ok(p) => p,
        Err(resp) => return *resp,
    };

    let response = (async move {
        let image_public = match state.store().get_image_public(&image_id).await {
            Ok(public) => public,
            Err(StoreError::NotFound) | Err(StoreError::InvalidImageId { .. }) => {
                return response_with_status(StatusCode::NOT_FOUND, &state, &req_headers);
            }
            Err(StoreError::Manifest(err)) => {
                state.metrics().inc_store_error("manifest");
                tracing::error!(error = %err, "store manifest error");
                return response_with_status(StatusCode::INTERNAL_SERVER_ERROR, &state, &req_headers);
            }
            Err(StoreError::Io(err)) => {
                state.metrics().inc_store_error("meta");
                tracing::error!(error = %err, "store get_image failed");
                return response_with_status(StatusCode::INTERNAL_SERVER_ERROR, &state, &req_headers);
            }
            Err(StoreError::InvalidRange { .. }) => {
                state.metrics().inc_store_error("meta");
                return response_with_status(StatusCode::INTERNAL_SERVER_ERROR, &state, &req_headers);
            }
        };

        let manifest = match state.store().open_chunked_manifest(&image_id).await {
            Ok(obj) => obj,
            Err(StoreError::NotFound) | Err(StoreError::InvalidImageId { .. }) => {
                return response_with_status(StatusCode::NOT_FOUND, &state, &req_headers);
            }
            Err(StoreError::Manifest(err)) => {
                state.metrics().inc_store_error("manifest");
                tracing::error!(error = %err, "store manifest error");
                return response_with_status(StatusCode::INTERNAL_SERVER_ERROR, &state, &req_headers);
            }
            Err(StoreError::Io(err)) => {
                state.metrics().inc_store_error("open_range");
                tracing::error!(error = %err, "store open_chunked_manifest failed");
                return response_with_status(StatusCode::INTERNAL_SERVER_ERROR, &state, &req_headers);
            }
            Err(StoreError::InvalidRange { .. }) => {
                state.metrics().inc_store_error("open_range");
                return response_with_status(StatusCode::INTERNAL_SERVER_ERROR, &state, &req_headers);
            }
        };

        let meta = manifest.meta;
        let cache_control = manifest_cache_control_value(&req_headers, image_public);
        let etag = cache::etag_header_value_for_meta(&meta);
        let current_etag = etag.to_str().ok();

        if cache::is_not_modified(&req_headers, current_etag, meta.last_modified) {
            return not_modified_response(&state, &req_headers, &meta, cache_control, etag);
        }

        let mut response = Response::new(if want_body {
            Body::from_stream(ReaderStream::new(manifest.reader))
        } else {
            Body::empty()
        });
        *response.status_mut() = StatusCode::OK;
        let headers = response.headers_mut();
        images::insert_cors_headers(headers, &state, &req_headers);
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static(crate::store::CONTENT_TYPE_JSON),
        );
        headers.insert(
            HeaderName::from_static("x-content-type-options"),
            HeaderValue::from_static("nosniff"),
        );
        headers.insert(
            header::CONTENT_LENGTH,
            HeaderValue::from_str(&meta.size.to_string()).unwrap(),
        );
        headers.insert(header::CACHE_CONTROL, cache_control);
        headers.insert(header::ETAG, etag);
        if let Some(last_modified) = cache::last_modified_header_value(meta.last_modified) {
            headers.insert(header::LAST_MODIFIED, last_modified);
        }
        response
    })
    .instrument(tracing::info_span!("chunked_manifest"))
    .await;

    images::attach_bytes_permit(response, permit)
}

async fn serve_manifest_version(
    image_id: String,
    version: String,
    state: ImagesState,
    req_headers: HeaderMap,
    want_body: bool,
) -> Response {
    let permit = match images::try_acquire_bytes_permit(&state, &req_headers) {
        Ok(p) => p,
        Err(resp) => return *resp,
    };

    let response = (async move {
        let image_public = match state.store().get_image_public(&image_id).await {
            Ok(public) => public,
            Err(StoreError::NotFound) | Err(StoreError::InvalidImageId { .. }) => {
                return response_with_status(StatusCode::NOT_FOUND, &state, &req_headers);
            }
            Err(StoreError::Manifest(err)) => {
                state.metrics().inc_store_error("manifest");
                tracing::error!(error = %err, "store manifest error");
                return response_with_status(StatusCode::INTERNAL_SERVER_ERROR, &state, &req_headers);
            }
            Err(StoreError::Io(err)) => {
                state.metrics().inc_store_error("meta");
                tracing::error!(error = %err, "store get_image failed");
                return response_with_status(StatusCode::INTERNAL_SERVER_ERROR, &state, &req_headers);
            }
            Err(StoreError::InvalidRange { .. }) => {
                state.metrics().inc_store_error("meta");
                return response_with_status(StatusCode::INTERNAL_SERVER_ERROR, &state, &req_headers);
            }
        };

        let manifest = match state
            .store()
            .open_chunked_manifest_version(&image_id, &version)
            .await
        {
            Ok(obj) => obj,
            Err(StoreError::NotFound) | Err(StoreError::InvalidImageId { .. }) => {
                return response_with_status(StatusCode::NOT_FOUND, &state, &req_headers);
            }
            Err(StoreError::Manifest(err)) => {
                state.metrics().inc_store_error("manifest");
                tracing::error!(error = %err, "store manifest error");
                return response_with_status(StatusCode::INTERNAL_SERVER_ERROR, &state, &req_headers);
            }
            Err(StoreError::Io(err)) => {
                state.metrics().inc_store_error("open_range");
                tracing::error!(error = %err, "store open_chunked_manifest_version failed");
                return response_with_status(StatusCode::INTERNAL_SERVER_ERROR, &state, &req_headers);
            }
            Err(StoreError::InvalidRange { .. }) => {
                state.metrics().inc_store_error("open_range");
                return response_with_status(StatusCode::INTERNAL_SERVER_ERROR, &state, &req_headers);
            }
        };

        let meta = manifest.meta;
        let cache_control = manifest_cache_control_value(&req_headers, image_public);
        let etag = cache::etag_header_value_for_meta(&meta);
        let current_etag = etag.to_str().ok();

        if cache::is_not_modified(&req_headers, current_etag, meta.last_modified) {
            return not_modified_response(&state, &req_headers, &meta, cache_control, etag);
        }

        let mut response = Response::new(if want_body {
            Body::from_stream(ReaderStream::new(manifest.reader))
        } else {
            Body::empty()
        });
        *response.status_mut() = StatusCode::OK;
        let headers = response.headers_mut();
        images::insert_cors_headers(headers, &state, &req_headers);
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static(crate::store::CONTENT_TYPE_JSON),
        );
        headers.insert(
            HeaderName::from_static("x-content-type-options"),
            HeaderValue::from_static("nosniff"),
        );
        headers.insert(
            header::CONTENT_LENGTH,
            HeaderValue::from_str(&meta.size.to_string()).unwrap(),
        );
        headers.insert(header::CACHE_CONTROL, cache_control);
        headers.insert(header::ETAG, etag);
        if let Some(last_modified) = cache::last_modified_header_value(meta.last_modified) {
            headers.insert(header::LAST_MODIFIED, last_modified);
        }
        response
    })
    .instrument(tracing::info_span!("chunked_manifest"))
    .await;

    images::attach_bytes_permit(response, permit)
}

pub async fn get_chunk(
    Path((image_id, chunk_name)): Path<(String, String)>,
    State(state): State<ImagesState>,
    headers: HeaderMap,
) -> Response {
    if crate::store::validate_image_id(&image_id).is_ok() {
        crate::http::observability::record_image_id(&image_id);
    } else {
        tracing::Span::current().record("image_id", tracing::field::display("<invalid>"));
        return response_with_status(StatusCode::NOT_FOUND, &state, &headers);
    }
    serve_chunk(image_id, chunk_name, state, headers, true).await
}

pub async fn get_chunk_version(
    Path((image_id, version, chunk_name)): Path<(String, String, String)>,
    State(state): State<ImagesState>,
    headers: HeaderMap,
) -> Response {
    if crate::store::validate_image_id(&image_id).is_ok()
        && crate::store::validate_image_id(&version).is_ok()
    {
        crate::http::observability::record_image_id(&image_id);
    } else {
        tracing::Span::current().record("image_id", tracing::field::display("<invalid>"));
        return response_with_status(StatusCode::NOT_FOUND, &state, &headers);
    }
    serve_chunk_version(image_id, version, chunk_name, state, headers, true).await
}

pub async fn head_chunk(
    Path((image_id, chunk_name)): Path<(String, String)>,
    State(state): State<ImagesState>,
    headers: HeaderMap,
) -> Response {
    if crate::store::validate_image_id(&image_id).is_ok() {
        crate::http::observability::record_image_id(&image_id);
    } else {
        tracing::Span::current().record("image_id", tracing::field::display("<invalid>"));
        return response_with_status(StatusCode::NOT_FOUND, &state, &headers);
    }
    serve_chunk(image_id, chunk_name, state, headers, false).await
}

pub async fn head_chunk_version(
    Path((image_id, version, chunk_name)): Path<(String, String, String)>,
    State(state): State<ImagesState>,
    headers: HeaderMap,
) -> Response {
    if crate::store::validate_image_id(&image_id).is_ok()
        && crate::store::validate_image_id(&version).is_ok()
    {
        crate::http::observability::record_image_id(&image_id);
    } else {
        tracing::Span::current().record("image_id", tracing::field::display("<invalid>"));
        return response_with_status(StatusCode::NOT_FOUND, &state, &headers);
    }
    serve_chunk_version(image_id, version, chunk_name, state, headers, false).await
}

async fn serve_chunk(
    image_id: String,
    chunk_name: String,
    state: ImagesState,
    req_headers: HeaderMap,
    want_body: bool,
) -> Response {
    let permit = match images::try_acquire_bytes_permit(&state, &req_headers) {
        Ok(p) => p,
        Err(resp) => return *resp,
    };

    let response = (async move {
        if !is_valid_chunk_name(&chunk_name) {
            return response_with_status(StatusCode::NOT_FOUND, &state, &req_headers);
        }

        let image_public = match state.store().get_image_public(&image_id).await {
            Ok(public) => public,
            Err(StoreError::NotFound) | Err(StoreError::InvalidImageId { .. }) => {
                return response_with_status(StatusCode::NOT_FOUND, &state, &req_headers);
            }
            Err(StoreError::Manifest(err)) => {
                state.metrics().inc_store_error("manifest");
                tracing::error!(error = %err, "store manifest error");
                return response_with_status(StatusCode::INTERNAL_SERVER_ERROR, &state, &req_headers);
            }
            Err(StoreError::Io(err)) => {
                state.metrics().inc_store_error("meta");
                tracing::error!(error = %err, "store get_image failed");
                return response_with_status(StatusCode::INTERNAL_SERVER_ERROR, &state, &req_headers);
            }
            Err(StoreError::InvalidRange { .. }) => {
                state.metrics().inc_store_error("meta");
                return response_with_status(StatusCode::INTERNAL_SERVER_ERROR, &state, &req_headers);
            }
        };

        let chunk = match state.store().open_chunked_chunk(&image_id, &chunk_name).await {
            Ok(obj) => obj,
            Err(StoreError::NotFound) | Err(StoreError::InvalidImageId { .. }) => {
                return response_with_status(StatusCode::NOT_FOUND, &state, &req_headers);
            }
            Err(StoreError::Manifest(err)) => {
                state.metrics().inc_store_error("manifest");
                tracing::error!(error = %err, "store manifest error");
                return response_with_status(StatusCode::INTERNAL_SERVER_ERROR, &state, &req_headers);
            }
            Err(StoreError::Io(err)) => {
                state.metrics().inc_store_error("open_range");
                tracing::error!(error = %err, "store open_chunked_chunk failed");
                return response_with_status(StatusCode::INTERNAL_SERVER_ERROR, &state, &req_headers);
            }
            Err(StoreError::InvalidRange { .. }) => {
                state.metrics().inc_store_error("open_range");
                return response_with_status(StatusCode::INTERNAL_SERVER_ERROR, &state, &req_headers);
            }
        };

        let meta = chunk.meta;

        // Abuse guard: cap the maximum chunk size to prevent pathological reads / amplification.
        let max_chunk_bytes = state.max_chunk_bytes();
        if meta.size > max_chunk_bytes {
            return response_with_status(StatusCode::PAYLOAD_TOO_LARGE, &state, &req_headers);
        }

        let cache_control = chunk_cache_control_value(&req_headers, image_public);
        let etag = cache::etag_header_value_for_meta(&meta);
        let current_etag = etag.to_str().ok();

        if cache::is_not_modified(&req_headers, current_etag, meta.last_modified) {
            return not_modified_response(&state, &req_headers, &meta, cache_control, etag);
        }

        if want_body {
            state.metrics().observe_image_bytes_served(&image_id, meta.size);
        }

        let mut response = Response::new(if want_body {
            Body::from_stream(ReaderStream::new(chunk.reader))
        } else {
            Body::empty()
        });
        *response.status_mut() = StatusCode::OK;
        let headers = response.headers_mut();
        images::insert_cors_headers(headers, &state, &req_headers);
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static(crate::store::CONTENT_TYPE_DISK_IMAGE),
        );
        headers.insert(header::CONTENT_ENCODING, HeaderValue::from_static("identity"));
        headers.insert(
            HeaderName::from_static("x-content-type-options"),
            HeaderValue::from_static("nosniff"),
        );
        headers.insert(
            header::CONTENT_LENGTH,
            HeaderValue::from_str(&meta.size.to_string()).unwrap(),
        );
        headers.insert(header::CACHE_CONTROL, cache_control);
        headers.insert(header::ETAG, etag);
        if let Some(last_modified) = cache::last_modified_header_value(meta.last_modified) {
            headers.insert(header::LAST_MODIFIED, last_modified);
        }
        response
    })
    .instrument(tracing::info_span!("chunked_chunk"))
    .await;

    images::attach_bytes_permit(response, permit)
}

async fn serve_chunk_version(
    image_id: String,
    version: String,
    chunk_name: String,
    state: ImagesState,
    req_headers: HeaderMap,
    want_body: bool,
) -> Response {
    let permit = match images::try_acquire_bytes_permit(&state, &req_headers) {
        Ok(p) => p,
        Err(resp) => return *resp,
    };

    let response = (async move {
        if !is_valid_chunk_name(&chunk_name) {
            return response_with_status(StatusCode::NOT_FOUND, &state, &req_headers);
        }

        let image_public = match state.store().get_image_public(&image_id).await {
            Ok(public) => public,
            Err(StoreError::NotFound) | Err(StoreError::InvalidImageId { .. }) => {
                return response_with_status(StatusCode::NOT_FOUND, &state, &req_headers);
            }
            Err(StoreError::Manifest(err)) => {
                state.metrics().inc_store_error("manifest");
                tracing::error!(error = %err, "store manifest error");
                return response_with_status(StatusCode::INTERNAL_SERVER_ERROR, &state, &req_headers);
            }
            Err(StoreError::Io(err)) => {
                state.metrics().inc_store_error("meta");
                tracing::error!(error = %err, "store get_image failed");
                return response_with_status(StatusCode::INTERNAL_SERVER_ERROR, &state, &req_headers);
            }
            Err(StoreError::InvalidRange { .. }) => {
                state.metrics().inc_store_error("meta");
                return response_with_status(StatusCode::INTERNAL_SERVER_ERROR, &state, &req_headers);
            }
        };

        let chunk = match state
            .store()
            .open_chunked_chunk_version(&image_id, &version, &chunk_name)
            .await
        {
            Ok(obj) => obj,
            Err(StoreError::NotFound) | Err(StoreError::InvalidImageId { .. }) => {
                return response_with_status(StatusCode::NOT_FOUND, &state, &req_headers);
            }
            Err(StoreError::Manifest(err)) => {
                state.metrics().inc_store_error("manifest");
                tracing::error!(error = %err, "store manifest error");
                return response_with_status(StatusCode::INTERNAL_SERVER_ERROR, &state, &req_headers);
            }
            Err(StoreError::Io(err)) => {
                state.metrics().inc_store_error("open_range");
                tracing::error!(error = %err, "store open_chunked_chunk_version failed");
                return response_with_status(StatusCode::INTERNAL_SERVER_ERROR, &state, &req_headers);
            }
            Err(StoreError::InvalidRange { .. }) => {
                state.metrics().inc_store_error("open_range");
                return response_with_status(StatusCode::INTERNAL_SERVER_ERROR, &state, &req_headers);
            }
        };

        let meta = chunk.meta;

        let max_chunk_bytes = state.max_chunk_bytes();
        if meta.size > max_chunk_bytes {
            return response_with_status(StatusCode::PAYLOAD_TOO_LARGE, &state, &req_headers);
        }

        let cache_control = chunk_cache_control_value(&req_headers, image_public);
        let etag = cache::etag_header_value_for_meta(&meta);
        let current_etag = etag.to_str().ok();

        if cache::is_not_modified(&req_headers, current_etag, meta.last_modified) {
            return not_modified_response(&state, &req_headers, &meta, cache_control, etag);
        }

        if want_body {
            state.metrics().observe_image_bytes_served(&image_id, meta.size);
        }

        let mut response = Response::new(if want_body {
            Body::from_stream(ReaderStream::new(chunk.reader))
        } else {
            Body::empty()
        });
        *response.status_mut() = StatusCode::OK;
        let headers = response.headers_mut();
        images::insert_cors_headers(headers, &state, &req_headers);
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static(crate::store::CONTENT_TYPE_DISK_IMAGE),
        );
        headers.insert(header::CONTENT_ENCODING, HeaderValue::from_static("identity"));
        headers.insert(
            HeaderName::from_static("x-content-type-options"),
            HeaderValue::from_static("nosniff"),
        );
        headers.insert(
            header::CONTENT_LENGTH,
            HeaderValue::from_str(&meta.size.to_string()).unwrap(),
        );
        headers.insert(header::CACHE_CONTROL, cache_control);
        headers.insert(header::ETAG, etag);
        if let Some(last_modified) = cache::last_modified_header_value(meta.last_modified) {
            headers.insert(header::LAST_MODIFIED, last_modified);
        }
        response
    })
    .instrument(tracing::info_span!("chunked_chunk"))
    .await;

    images::attach_bytes_permit(response, permit)
}

fn is_valid_chunk_name(name: &str) -> bool {
    if name.len() != CHUNK_NAME_LEN {
        return false;
    }
    let bytes = name.as_bytes();
    // First 8 chars must be ASCII digits.
    if !bytes[..8].iter().all(|b| b.is_ascii_digit()) {
        return false;
    }
    bytes[8..] == *b".bin"
}

fn manifest_cache_control_value(req_headers: &HeaderMap, image_public: bool) -> HeaderValue {
    if !image_public {
        return HeaderValue::from_static("private, no-store, no-transform");
    }
    if req_headers.contains_key(header::AUTHORIZATION) || req_headers.contains_key(header::COOKIE) {
        HeaderValue::from_static("private, no-store, no-transform")
    } else {
        HeaderValue::from_static(PUBLIC_CACHE_CONTROL_MANIFEST)
    }
}

fn chunk_cache_control_value(req_headers: &HeaderMap, image_public: bool) -> HeaderValue {
    if !image_public {
        return HeaderValue::from_static("private, no-store, no-transform");
    }
    if req_headers.contains_key(header::AUTHORIZATION) || req_headers.contains_key(header::COOKIE) {
        HeaderValue::from_static("private, no-store, no-transform")
    } else {
        HeaderValue::from_static(PUBLIC_CACHE_CONTROL_CHUNKS)
    }
}

fn not_modified_response(
    state: &ImagesState,
    req_headers: &HeaderMap,
    meta: &ImageMeta,
    cache_control: HeaderValue,
    etag: HeaderValue,
) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::NOT_MODIFIED;
    let headers = response.headers_mut();
    images::insert_cors_headers(headers, state, req_headers);
    headers.insert(header::CACHE_CONTROL, cache_control);
    headers.insert(header::ETAG, etag);
    if let Some(last_modified) = cache::last_modified_header_value(meta.last_modified) {
        headers.insert(header::LAST_MODIFIED, last_modified);
    }
    response
}

fn response_with_status(status: StatusCode, state: &ImagesState, req_headers: &HeaderMap) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = status;
    let headers = response.headers_mut();
    images::insert_cors_headers(headers, state, req_headers);
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, no-transform"),
    );
    response
}
