pub mod cache;
pub mod images;
pub mod range;

mod metrics;
pub(crate) mod observability;

use std::sync::Arc;

use axum::{routing::get, Router};

use crate::{metrics::Metrics, store::ImageStore};

pub fn router(store: Arc<dyn ImageStore>, metrics: Arc<Metrics>) -> Router {
    router_with_state(images::ImagesState::new(store, metrics))
}

pub fn router_with_state(state: images::ImagesState) -> Router {
    let router = Router::<images::ImagesState>::new()
        .merge(images::router())
        // DoS hardening: reject pathological `:image_id` segments before `Path<String>` extraction
        // to avoid allocating attacker-controlled huge IDs.
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            images::image_id_path_len_guard,
        ));
    let router = if state.metrics_endpoint_disabled() {
        router
    } else {
        router.route("/metrics", get(metrics::handle))
    };
    router.with_state(state)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::{
        body::Body,
        http::{Request, StatusCode},
        middleware,
    };
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::{http, metrics::Metrics, store::LocalFsImageStore};

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn metrics_endpoint_smoke_test() {
        let store = Arc::new(LocalFsImageStore::new("."));
        let metrics = Arc::new(Metrics::new());
        let app = http::router(store, Arc::clone(&metrics)).route_layer(
            middleware::from_fn_with_state(Arc::clone(&metrics), http::observability::middleware),
        );

        // First request initializes per-route labels in the middleware; second request verifies
        // they show up in the exposition.
        let _ = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()["access-control-allow-origin"]
                .to_str()
                .unwrap(),
            "*"
        );

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8(body.to_vec()).unwrap();

        assert!(text
            .contains("http_requests_total{method=\"GET\",route=\"/metrics\",status=\"200\"} 1"));
        assert!(text.contains("range_requests_total"));
    }
}
