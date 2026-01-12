use axum::{
    extract::State,
    http::{header, HeaderValue},
    http::HeaderMap,
    response::{IntoResponse, Response},
};

use super::images::ImagesState;

pub(crate) async fn handle(State(state): State<ImagesState>, req_headers: HeaderMap) -> Response {
    let body = state.metrics().encode();
    let mut resp = (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static(crate::metrics::Metrics::metrics_content_type()),
        )],
        body,
    )
        .into_response();
    super::images::insert_cors_headers(resp.headers_mut(), &state, &req_headers);
    resp
}
