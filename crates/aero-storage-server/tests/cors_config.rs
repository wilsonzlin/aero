use aero_storage_server::{store::LocalFsImageStore, AppState};
use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};
use std::sync::Arc;
use tempfile::tempdir;
use tower::ServiceExt;

#[tokio::test]
async fn cors_origin_override_is_applied_to_metadata_and_bytes_endpoints() {
    let dir = tempdir().expect("tempdir");
    tokio::fs::write(dir.path().join("test.img"), b"Hello, world!")
        .await
        .expect("write test file");

    let store = Arc::new(LocalFsImageStore::new(dir.path()));
    let state = AppState::new(store)
        .with_cors_allow_origin("https://example.com".parse().unwrap())
        .with_cors_allow_credentials(true);
    let app = aero_storage_server::app(state);

    for (name, uri) in [
        ("bytes", "/v1/images/test.img"),
        ("meta", "/v1/images/test.img/meta"),
    ] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(uri)
                    .header(header::ORIGIN, "https://example.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK, "{name}");
        assert_eq!(
            resp.headers()["access-control-allow-origin"].to_str().unwrap(),
            "https://example.com",
            "{name}"
        );
        assert_eq!(
            resp.headers()["access-control-allow-credentials"]
                .to_str()
                .unwrap(),
            "true",
            "{name}"
        );
    }
}

