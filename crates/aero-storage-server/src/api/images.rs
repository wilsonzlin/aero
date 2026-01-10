use axum::extract::{Path, State};
use axum::http::header::{CACHE_CONTROL, HeaderValue};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};

use crate::store::{ImageCatalogEntry, StoreError};
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/images", get(list_images))
        .route("/v1/images/:id/meta", get(get_image_meta))
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
    headers
}

pub async fn list_images(State(state): State<AppState>) -> Result<impl IntoResponse, ApiError> {
    let images = state.store.list_images().await?;
    let images: Vec<ImageResponse> = images.into_iter().map(ImageResponse::from).collect();
    Ok((metadata_cache_headers(), Json(images)))
}

pub async fn get_image_meta(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, ApiError> {
    let image = state.store.get_image(&id).await?;
    Ok((metadata_cache_headers(), Json(ImageResponse::from(image))))
}
