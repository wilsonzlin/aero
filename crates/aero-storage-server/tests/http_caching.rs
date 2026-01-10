use aero_storage_server::{store::LocalFsImageStore, AppState};
use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};
use http_body_util::BodyExt;
use std::sync::Arc;
use tempfile::tempdir;
use tower::ServiceExt;

async fn setup_app() -> (axum::Router, tempfile::TempDir) {
    let dir = tempdir().expect("tempdir");
    tokio::fs::write(dir.path().join("test.img"), b"Hello, world!")
        .await
        .expect("write test file");

    let store = Arc::new(LocalFsImageStore::new(dir.path()));
    let state = AppState::new(store);
    (aero_storage_server::app(state), dir)
}

#[tokio::test]
async fn get_image_meta_with_if_none_match_returns_304() {
    let (app, _dir) = setup_app().await;

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/test.img/meta")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let etag = res.headers()[header::ETAG].to_str().unwrap().to_string();

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/test.img/meta")
                .header(header::IF_NONE_MATCH, etag)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
}

#[tokio::test]
async fn head_image_data_includes_etag_last_modified_and_accept_ranges() {
    let (app, _dir) = setup_app().await;

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::HEAD)
                .uri("/v1/images/test.img/data")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    assert!(res.headers().contains_key(header::ETAG));
    assert!(res.headers().contains_key(header::LAST_MODIFIED));
    assert_eq!(
        res.headers()[header::ACCEPT_RANGES].to_str().unwrap(),
        "bytes"
    );

    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert!(body.is_empty());
}
