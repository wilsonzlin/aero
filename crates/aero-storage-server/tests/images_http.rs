#![cfg(not(target_arch = "wasm32"))]

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

async fn setup_app(max_total_bytes: u64) -> (axum::Router, tempfile::TempDir) {
    let dir = tempdir().expect("tempdir");
    tokio::fs::write(dir.path().join("test.img"), b"Hello, world!")
        .await
        .expect("write test file");

    let store = Arc::new(LocalFsImageStore::new(dir.path()));
    let metrics = Arc::new(Metrics::new());
    let state =
        ImagesState::new(store, metrics).with_range_options(RangeOptions { max_total_bytes });
    (router_with_state(state), dir)
}

#[tokio::test]
async fn get_without_range_returns_full_body() {
    let (app, _dir) = setup_app(1024).await;

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
    assert_eq!(
        res.headers()["cross-origin-resource-policy"]
            .to_str()
            .unwrap(),
        "same-site"
    );

    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"Hello, world!");
}

#[tokio::test]
async fn overly_long_image_id_is_rejected_with_404() {
    let (app, dir) = setup_app(1024).await;

    // > `MAX_IMAGE_ID_LEN` should be rejected by the store validator even if a file exists.
    let long_id = "a".repeat(aero_storage_server::store::MAX_IMAGE_ID_LEN + 1);
    tokio::fs::write(dir.path().join(&long_id), b"x")
        .await
        .expect("write long id file");

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/v1/images/{long_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    // The legacy `/data` alias should behave the same.
    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/v1/images/{long_id}/data"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn request_with_cookie_is_not_publicly_cacheable() {
    let (app, _dir) = setup_app(1024).await;

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/test.img")
                .header(header::COOKIE, "aero_session=deadbeef")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()[header::CACHE_CONTROL].to_str().unwrap(),
        "private, no-store, no-transform"
    );
}

#[tokio::test]
async fn head_without_range_returns_headers_only() {
    let (app, _dir) = setup_app(1024).await;

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
    let (app, _dir) = setup_app(1024).await;

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
async fn range_with_if_range_matching_etag_returns_206() {
    let (app, _dir) = setup_app(1024).await;

    let head = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::HEAD)
                .uri("/v1/images/test.img")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let etag = head.headers()[header::ETAG].to_str().unwrap().to_string();
    assert!(
        !etag.starts_with("W/"),
        "If-Range support requires a strong ETag, got {etag:?}"
    );

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/test.img")
                .header(header::RANGE, "bytes=0-0")
                .header(header::IF_RANGE, etag)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"H");
}

#[tokio::test]
async fn range_with_if_range_mismatch_returns_200() {
    let (app, _dir) = setup_app(1024).await;

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/test.img")
                .header(header::RANGE, "bytes=0-0")
                .header(header::IF_RANGE, "\"mismatch\"")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"Hello, world!");
}

#[tokio::test]
async fn range_unsatisfiable_returns_416_with_content_range_star() {
    let (app, _dir) = setup_app(1024).await;

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
async fn range_multiple_is_rejected_with_416() {
    let (app, _dir) = setup_app(1024).await;

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

    assert_eq!(res.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(
        res.headers()[header::CONTENT_RANGE].to_str().unwrap(),
        "bytes */13"
    );
}

#[tokio::test]
async fn range_abuse_guard_returns_413() {
    let (app, _dir) = setup_app(1).await;

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/test.img")
                .header(header::RANGE, "bytes=0-1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn range_header_too_large_returns_413() {
    let (app, _dir) = setup_app(1024).await;

    let mut range = String::from("bytes=0-0");
    range.push_str(&"0".repeat(aero_http_range::MAX_RANGE_HEADER_LEN));

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/test.img")
                .header(header::RANGE, range)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn range_header_too_many_ranges_returns_413() {
    let (app, _dir) = setup_app(1024).await;

    let parts = vec!["0-0"; aero_http_range::MAX_RANGE_SPECS + 1];
    let range = format!("bytes={}", parts.join(","));

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/test.img")
                .header(header::RANGE, range)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn cors_preflight_allows_range() {
    let (app, _dir) = setup_app(1024).await;

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

#[cfg(unix)]
#[tokio::test]
async fn symlink_escape_is_blocked() {
    use std::os::unix::fs::symlink;

    let root_dir = tempdir().expect("tempdir");
    let outside_dir = tempdir().expect("tempdir");

    let outside_path = outside_dir.path().join("secret.img");
    let sentinel = b"TOP-SECRET";
    tokio::fs::write(&outside_path, sentinel)
        .await
        .expect("write outside file");

    let link_name = "leak.img";
    symlink(&outside_path, root_dir.path().join(link_name)).expect("create symlink");

    let store = Arc::new(LocalFsImageStore::new(root_dir.path()));
    let metrics = Arc::new(Metrics::new());
    let state = ImagesState::new(store, metrics).with_range_options(RangeOptions {
        max_total_bytes: 1024,
    });
    let app = router_with_state(state);

    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/v1/images/{link_name}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert!(
        body.is_empty(),
        "expected empty 404 response body; got {body:?}"
    );
}
