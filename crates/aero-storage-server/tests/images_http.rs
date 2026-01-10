use aero_storage_server::{
    http::{
        images::{router_with_state, ImagesState},
        range::RangeOptions,
    },
    metrics::Metrics,
    store::LocalFsImageStore,
};
use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};
use http_body_util::BodyExt;
use std::sync::Arc;
use tempfile::tempdir;
use tower::ServiceExt;

async fn setup_app(max_ranges: usize, max_total_bytes: u64) -> (axum::Router, tempfile::TempDir) {
    let dir = tempdir().expect("tempdir");
    tokio::fs::write(dir.path().join("test.img"), b"Hello, world!")
        .await
        .expect("write test file");

    let store = Arc::new(LocalFsImageStore::new(dir.path()));
    let metrics = Arc::new(Metrics::new());
    let state = ImagesState::new(store, metrics).with_range_options(RangeOptions {
        max_ranges,
        max_total_bytes,
    });
    (router_with_state(state), dir)
}

#[tokio::test]
async fn get_without_range_returns_full_body() {
    let (app, _dir) = setup_app(16, 1024).await;

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/test.img")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()[header::ACCEPT_RANGES].to_str().unwrap(),
        "bytes"
    );
    assert_eq!(
        res.headers()[header::CONTENT_TYPE].to_str().unwrap(),
        "application/octet-stream"
    );
    assert_eq!(
        res.headers()[header::CONTENT_LENGTH].to_str().unwrap(),
        "13"
    );

    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"Hello, world!");
}

#[tokio::test]
async fn head_without_range_returns_headers_only() {
    let (app, _dir) = setup_app(16, 1024).await;

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::HEAD)
                .uri("/v1/images/test.img")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()[header::CONTENT_LENGTH].to_str().unwrap(),
        "13"
    );

    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert!(body.is_empty());
}

#[tokio::test]
async fn range_single_byte_returns_206() {
    let (app, _dir) = setup_app(16, 1024).await;

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/test.img")
                .header(header::RANGE, "bytes=0-0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        res.headers()[header::CONTENT_RANGE].to_str().unwrap(),
        "bytes 0-0/13"
    );
    assert_eq!(res.headers()[header::CONTENT_LENGTH].to_str().unwrap(), "1");

    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"H");
}

#[tokio::test]
async fn range_unsatisfiable_returns_416_with_content_range_star() {
    let (app, _dir) = setup_app(16, 1024).await;

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/test.img")
                .header(header::RANGE, "bytes=999-1000")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(
        res.headers()[header::CONTENT_RANGE].to_str().unwrap(),
        "bytes */13"
    );
}

#[tokio::test]
async fn range_multiple_returns_multipart_body() {
    let (app, _dir) = setup_app(16, 1024).await;

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/test.img")
                .header(header::RANGE, "bytes=0-0,2-2")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);

    let content_type = res.headers()[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .to_string();
    assert!(content_type.starts_with("multipart/byteranges; boundary="));
    let boundary = content_type
        .split("boundary=")
        .nth(1)
        .expect("boundary token")
        .to_string();

    let body = res.into_body().collect().await.unwrap().to_bytes();
    let body_str = String::from_utf8(body.to_vec()).unwrap();

    let expected = format!(
        "--{b}\r\nContent-Type: application/octet-stream\r\nContent-Range: bytes 0-0/13\r\n\r\nH\r\n--{b}\r\nContent-Type: application/octet-stream\r\nContent-Range: bytes 2-2/13\r\n\r\nl\r\n--{b}--\r\n",
        b = boundary
    );
    assert_eq!(body_str, expected);
}

#[tokio::test]
async fn range_abuse_guard_returns_413() {
    let (app, _dir) = setup_app(1, 1).await;

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/test.img")
                .header(header::RANGE, "bytes=0-0,2-2")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn cors_preflight_allows_range() {
    let (app, _dir) = setup_app(16, 1024).await;

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/v1/images/test.img")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        res.headers()["access-control-allow-origin"]
            .to_str()
            .unwrap(),
        "*"
    );
    assert!(res.headers()["access-control-allow-headers"]
        .to_str()
        .unwrap()
        .contains("Range"));
}
