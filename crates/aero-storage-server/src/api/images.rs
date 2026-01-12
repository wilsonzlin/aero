use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::header::{self, CACHE_CONTROL, ETAG, LAST_MODIFIED};
use axum::http::HeaderMap;
use axum::http::Request;
use axum::http::StatusCode;
use axum::http::HeaderValue;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};

use crate::http::cache;
use crate::store::{ImageCatalogEntry, StoreError};
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/images",
            get(list_images).head(head_images).options(options_images),
        )
        .route(
            "/v1/images/:id/meta",
            get(get_image_meta)
                .head(head_image_meta)
                .options(options_image_meta),
        )
        // DoS hardening: reject pathological `:id` segments before `Path<String>` extraction.
        .route_layer(axum::middleware::from_fn(image_id_path_len_guard))
}

async fn image_id_path_len_guard(req: Request<Body>, next: Next) -> Response {
    let path = req.uri().path();
    let Some(rest) = path.strip_prefix("/v1/images/") else {
        return next.run(req).await;
    };
    let Some(rest) = rest.strip_suffix("/meta") else {
        return next.run(req).await;
    };

    // Only enforce on `/v1/images/:id/meta`.
    if rest.contains('/') {
        return next.run(req).await;
    }

    // A percent-encoded byte takes 3 chars (`%xx`), so if the raw path segment exceeds
    // `MAX_IMAGE_ID_LEN * 3` then the decoded ID must exceed `MAX_IMAGE_ID_LEN` as well.
    if rest.len() > crate::store::MAX_IMAGE_ID_LEN * 3 {
        return StatusCode::BAD_REQUEST.into_response();
    }

    next.run(req).await
}

#[derive(Debug, serde::Serialize)]
struct ImageResponse {
    id: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    size_bytes: u64,
    etag: String,
    last_modified: String,
    content_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    recommended_chunk_size_bytes: Option<u64>,
    public: bool,
}

impl From<ImageCatalogEntry> for ImageResponse {
    fn from(entry: ImageCatalogEntry) -> Self {
        let last_modified = entry
            .meta
            .last_modified
            .and_then(|t| {
                time::OffsetDateTime::from(t)
                    .format(&time::format_description::well_known::Rfc3339)
                    .ok()
            })
            .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string());

        Self {
            id: entry.id,
            name: entry.name,
            description: entry.description,
            size_bytes: entry.meta.size,
            etag: cache::etag_or_fallback(&entry.meta),
            last_modified,
            content_type: entry.meta.content_type.to_string(),
            recommended_chunk_size_bytes: entry.recommended_chunk_size_bytes,
            public: entry.public,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("image not found")]
    NotFound,
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error(transparent)]
    Store(StoreError),
}

impl From<StoreError> for ApiError {
    fn from(err: StoreError) -> Self {
        match err {
            StoreError::NotFound => ApiError::NotFound,
            StoreError::InvalidImageId { image_id } => {
                ApiError::BadRequest(format!("invalid image id: {image_id:?}"))
            }
            StoreError::InvalidRange { .. } => ApiError::BadRequest(err.to_string()),
            other => ApiError::Store(other),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self {
            ApiError::NotFound => axum::http::StatusCode::NOT_FOUND,
            ApiError::BadRequest(_) => axum::http::StatusCode::BAD_REQUEST,
            ApiError::Store(_) => axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, self.to_string()).into_response()
    }
}

fn metadata_cache_headers(state: &AppState, req_headers: &HeaderMap) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    state.cors.insert_cors_headers(
        &mut headers,
        req_headers,
        Some(HeaderValue::from_static("ETag, Last-Modified, Cache-Control")),
    );
    headers
}

fn insert_metadata_preflight_headers(
    headers: &mut HeaderMap,
    state: &AppState,
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
}

pub async fn options_images(State(state): State<AppState>, req_headers: HeaderMap) -> Response {
    let mut resp = StatusCode::NO_CONTENT.into_response();
    insert_metadata_preflight_headers(resp.headers_mut(), &state, &req_headers);
    resp
}

pub async fn options_image_meta(State(state): State<AppState>, req_headers: HeaderMap) -> Response {
    let mut resp = StatusCode::NO_CONTENT.into_response();
    insert_metadata_preflight_headers(resp.headers_mut(), &state, &req_headers);
    resp
}

pub async fn list_images(
    State(state): State<AppState>,
    req_headers: HeaderMap,
) -> Result<Response, ApiError> {
    let images = state.store.list_images().await?;
    let etag_entries: Vec<(String, crate::store::ImageMeta)> = images
        .iter()
        .map(|img| (img.id.clone(), img.meta.clone()))
        .collect();
    let list_etag = cache::etag_for_image_list(&etag_entries);

    if cache::is_not_modified(&req_headers, list_etag.to_str().ok(), None) {
        let mut headers = metadata_cache_headers(&state, &req_headers);
        headers.insert(ETAG, list_etag);
        let mut resp = StatusCode::NOT_MODIFIED.into_response();
        *resp.headers_mut() = headers;
        return Ok(resp);
    }

    let images: Vec<ImageResponse> = images.into_iter().map(ImageResponse::from).collect();
    let mut headers = metadata_cache_headers(&state, &req_headers);
    headers.insert(ETAG, list_etag);
    Ok((headers, Json(images)).into_response())
}

pub async fn get_image_meta(
    Path(id): Path<String>,
    State(state): State<AppState>,
    req_headers: HeaderMap,
) -> Result<Response, ApiError> {
    let image = state.store.get_image(&id).await?;
    let etag = cache::etag_header_value_for_meta(&image.meta);

    if cache::is_not_modified(&req_headers, etag.to_str().ok(), image.meta.last_modified) {
        let mut headers = metadata_cache_headers(&state, &req_headers);
        headers.insert(ETAG, etag);
        if let Some(lm) = cache::last_modified_header_value(image.meta.last_modified) {
            headers.insert(LAST_MODIFIED, lm);
        }
        let mut resp = StatusCode::NOT_MODIFIED.into_response();
        *resp.headers_mut() = headers;
        return Ok(resp);
    }

    let mut headers = metadata_cache_headers(&state, &req_headers);
    headers.insert(ETAG, etag);
    if let Some(lm) = cache::last_modified_header_value(image.meta.last_modified) {
        headers.insert(LAST_MODIFIED, lm);
    }
    Ok((headers, Json(ImageResponse::from(image))).into_response())
}

pub async fn head_images(
    State(state): State<AppState>,
    req_headers: HeaderMap,
) -> Result<Response, ApiError> {
    let images = state.store.list_images().await?;
    let etag_entries: Vec<(String, crate::store::ImageMeta)> = images
        .iter()
        .map(|img| (img.id.clone(), img.meta.clone()))
        .collect();
    let list_etag = cache::etag_for_image_list(&etag_entries);

    if cache::is_not_modified(&req_headers, list_etag.to_str().ok(), None) {
        let mut headers = metadata_cache_headers(&state, &req_headers);
        headers.insert(ETAG, list_etag);
        let mut resp = StatusCode::NOT_MODIFIED.into_response();
        *resp.headers_mut() = headers;
        return Ok(resp);
    }

    let mut headers = metadata_cache_headers(&state, &req_headers);
    headers.insert(ETAG, list_etag);
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    Ok((headers, ()).into_response())
}

pub async fn head_image_meta(
    Path(id): Path<String>,
    State(state): State<AppState>,
    req_headers: HeaderMap,
) -> Result<Response, ApiError> {
    let image = state.store.get_image(&id).await?;
    let etag = cache::etag_header_value_for_meta(&image.meta);

    if cache::is_not_modified(&req_headers, etag.to_str().ok(), image.meta.last_modified) {
        let mut headers = metadata_cache_headers(&state, &req_headers);
        headers.insert(ETAG, etag);
        if let Some(lm) = cache::last_modified_header_value(image.meta.last_modified) {
            headers.insert(LAST_MODIFIED, lm);
        }
        let mut resp = StatusCode::NOT_MODIFIED.into_response();
        *resp.headers_mut() = headers;
        return Ok(resp);
    }

    let mut headers = metadata_cache_headers(&state, &req_headers);
    headers.insert(ETAG, etag);
    if let Some(lm) = cache::last_modified_header_value(image.meta.last_modified) {
        headers.insert(LAST_MODIFIED, lm);
    }
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    Ok((headers, ()).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{ImageCatalogEntry, ImageMeta, CONTENT_TYPE_DISK_IMAGE};
    use std::time::Duration;

    #[test]
    fn image_response_includes_fallback_etag_when_missing() {
        let meta = ImageMeta {
            size: 123,
            etag: None,
            last_modified: Some(std::time::UNIX_EPOCH + Duration::from_secs(1)),
            content_type: CONTENT_TYPE_DISK_IMAGE,
        };
        let entry = ImageCatalogEntry {
            id: "disk".to_string(),
            name: "Disk".to_string(),
            description: None,
            recommended_chunk_size_bytes: None,
            public: true,
            meta: meta.clone(),
        };

        let response = ImageResponse::from(entry);
        assert_eq!(response.etag, cache::etag_or_fallback(&meta));
        assert!(!response.etag.is_empty());
    }
}
