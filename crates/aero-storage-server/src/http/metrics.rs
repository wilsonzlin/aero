use axum::{
    extract::State,
    http::{header, HeaderValue},
    response::IntoResponse,
};

use super::images::ImagesState;

pub(crate) async fn handle(State(state): State<ImagesState>) -> impl IntoResponse {
    let body = state.metrics().encode();
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static(crate::metrics::Metrics::metrics_content_type()),
        )],
        body,
    )
}
