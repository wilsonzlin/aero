use axum::{
    extract::State,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};

use super::images::ImagesState;

pub(crate) async fn handle(State(state): State<ImagesState>, req_headers: HeaderMap) -> Response {
    if let Some(expected_token) = state.metrics_auth_token() {
        match req_headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(bearer_token)
        {
            Some(token) if constant_time_eq(token, expected_token) => {}
            Some(_) => return forbidden(&state, &req_headers),
            None => return unauthorized(&state, &req_headers),
        }
    }

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

fn bearer_token(header_value: &str) -> Option<&str> {
    let mut parts = header_value.split_whitespace();
    let scheme = parts.next()?;
    let token = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    Some(token)
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }

    let mut diff = 0u8;
    for (x, y) in a.as_bytes().iter().zip(b.as_bytes().iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn unauthorized(state: &ImagesState, req_headers: &HeaderMap) -> Response {
    let mut resp = StatusCode::UNAUTHORIZED.into_response();
    resp.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Bearer"),
    );
    super::images::insert_cors_headers(resp.headers_mut(), state, req_headers);
    resp
}

fn forbidden(state: &ImagesState, req_headers: &HeaderMap) -> Response {
    let mut resp = StatusCode::FORBIDDEN.into_response();
    super::images::insert_cors_headers(resp.headers_mut(), state, req_headers);
    resp
}

