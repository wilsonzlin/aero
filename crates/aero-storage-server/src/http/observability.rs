use std::{sync::Arc, time::Instant};

use axum::{
    body::Body,
    extract::MatchedPath,
    extract::State,
    http::{header, HeaderValue, Request},
    middleware::Next,
    response::Response,
};
use uuid::Uuid;

use crate::metrics::Metrics;

pub(crate) fn truncate_for_span(value: &str, max_len: usize) -> std::borrow::Cow<'_, str> {
    if value.len() <= max_len {
        return std::borrow::Cow::Borrowed(value);
    }

    const ELLIPSIS: &str = "...";
    if max_len <= ELLIPSIS.len() {
        return std::borrow::Cow::Borrowed(&ELLIPSIS[..max_len]);
    }

    // Truncate at a valid UTF-8 boundary.
    let mut end = max_len - ELLIPSIS.len();
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }

    let mut out = value[..end].to_string();
    out.push_str(ELLIPSIS);
    std::borrow::Cow::Owned(out)
}

pub(crate) fn record_image_id(image_id: &str) {
    // `image_id` originates from the URL path; keep it bounded in tracing fields to avoid
    // excessive allocations/log spam in case validation is bypassed elsewhere.
    let image_id = truncate_for_span(image_id, crate::store::MAX_IMAGE_ID_LEN);
    tracing::Span::current().record("image_id", tracing::field::display(&image_id));
}

pub(crate) async fn middleware(
    State(metrics): State<Arc<Metrics>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let request_id = req
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= 128)
        .map(|value| value.to_string())
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|matched| matched.as_str())
        .unwrap_or("<unknown>")
        .to_string();

    let method = req.method().clone();
    let path = truncate_for_span(req.uri().path(), 256);
    let user_id = req
        .headers()
        .get("x-aero-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= 128);

    let span = tracing::info_span!(
        "http_request",
        request_id = %request_id,
        method = %method.as_str(),
        path = %path,
        route = %route,
        user_id = tracing::field::Empty,
        image_id = tracing::field::Empty,
        store_read_seconds = tracing::field::Empty,
    );
    if let Some(user_id) = user_id {
        span.record("user_id", tracing::field::display(user_id));
    }

    let start = Instant::now();
    let mut res = {
        let _guard = span.enter();
        next.run(req).await
    };

    let latency = start.elapsed();
    let status = res.status().as_u16();
    let bytes_sent = res
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);

    res.headers_mut().insert(
        "x-request-id",
        HeaderValue::from_str(&request_id).unwrap_or_else(|_| HeaderValue::from_static("invalid")),
    );

    metrics.observe_http_request(&route, method.as_str(), status, latency);

    tracing::info!(
        parent: &span,
        status,
        bytes_sent,
        latency_seconds = latency.as_secs_f64(),
        "request complete"
    );

    res
}
