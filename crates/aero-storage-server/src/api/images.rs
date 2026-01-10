use axum::extract::{Path, State};
use axum::http::header::{self, CACHE_CONTROL, ETAG, LAST_MODIFIED, VARY};
use axum::http::{HeaderName, HeaderValue};
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};

use crate::http::cache;
use crate::store::{ImageCatalogEntry, StoreError};
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/images", get(list_images).head(head_images))
        .route(
            "/v1/images/:id/meta",
            get(get_image_meta).head(head_image_meta),
        )
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
            etag: entry.meta.etag.unwrap_or_default(),
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

fn metadata_cache_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert(
        HeaderName::from_static("access-control-allow-origin"),
        HeaderValue::from_static("*"),
    );
    headers.insert(
        HeaderName::from_static("access-control-expose-headers"),
        HeaderValue::from_static("ETag, Last-Modified, Cache-Control"),
    );
    headers.insert(VARY, HeaderValue::from_static("Origin"));
    headers
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
        let mut headers = metadata_cache_headers();
        headers.insert(ETAG, list_etag);
        let mut resp = StatusCode::NOT_MODIFIED.into_response();
        *resp.headers_mut() = headers;
        return Ok(resp);
    }

    let images: Vec<ImageResponse> = images.into_iter().map(ImageResponse::from).collect();
    let mut headers = metadata_cache_headers();
    headers.insert(ETAG, list_etag);
    Ok((headers, Json(images)).into_response())
}

pub async fn get_image_meta(
    Path(id): Path<String>,
    State(state): State<AppState>,
    req_headers: HeaderMap,
) -> Result<Response, ApiError> {
    let image = state.store.get_image(&id).await?;
    let etag = cache::etag_or_fallback(&image.meta);

    if cache::is_not_modified(&req_headers, Some(&etag), image.meta.last_modified) {
        let mut headers = metadata_cache_headers();
        headers.insert(ETAG, HeaderValue::from_str(&etag).unwrap());
        if let Some(lm) = cache::last_modified_header_value(image.meta.last_modified) {
            headers.insert(LAST_MODIFIED, lm);
        }
        let mut resp = StatusCode::NOT_MODIFIED.into_response();
        *resp.headers_mut() = headers;
        return Ok(resp);
    }

    let mut headers = metadata_cache_headers();
    headers.insert(ETAG, HeaderValue::from_str(&etag).unwrap());
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
        let mut headers = metadata_cache_headers();
        headers.insert(ETAG, list_etag);
        let mut resp = StatusCode::NOT_MODIFIED.into_response();
        *resp.headers_mut() = headers;
        return Ok(resp);
    }

    let mut headers = metadata_cache_headers();
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
    let etag = cache::etag_or_fallback(&image.meta);

    if cache::is_not_modified(&req_headers, Some(&etag), image.meta.last_modified) {
        let mut headers = metadata_cache_headers();
        headers.insert(ETAG, HeaderValue::from_str(&etag).unwrap());
        if let Some(lm) = cache::last_modified_header_value(image.meta.last_modified) {
            headers.insert(LAST_MODIFIED, lm);
        }
        let mut resp = StatusCode::NOT_MODIFIED.into_response();
        *resp.headers_mut() = headers;
        return Ok(resp);
    }

    let mut headers = metadata_cache_headers();
    headers.insert(ETAG, HeaderValue::from_str(&etag).unwrap());
    if let Some(lm) = cache::last_modified_header_value(image.meta.last_modified) {
        headers.insert(LAST_MODIFIED, lm);
    }
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    Ok((headers, ()).into_response())
}
