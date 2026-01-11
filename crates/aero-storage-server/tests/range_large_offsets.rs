use aero_storage_server::http::{
    images::{router_with_state, ImagesState},
    range::RangeOptions,
};
use aero_storage_server::metrics::Metrics;
use aero_storage_server::store::LocalFsImageStore;
use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};
use http_body_util::BodyExt;
use std::sync::Arc;
use tempfile::tempdir;
use tokio::io::{AsyncSeekExt, AsyncWriteExt, SeekFrom};
use tower::ServiceExt;

const FILE_SIZE: u64 = 5_368_709_120; // 5 GiB
const HIGH_OFFSET: u64 = 4_294_967_296 + 123; // 2^32 + 123

const SENTINEL_HIGH: &[u8] = b"AERO_RANGE_4GB";
const SENTINEL_END: &[u8] = b"AERO_RANGE_END";

#[tokio::test]
async fn http_range_supports_offsets_beyond_4gib_and_suffix_ranges() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("large.img");

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&path)
        .await
        .expect("create image");

    // Create a sparse 5GiB image (does not allocate 5GiB on disk on typical filesystems).
    file.set_len(FILE_SIZE).await.expect("set_len");

    file.seek(SeekFrom::Start(HIGH_OFFSET))
        .await
        .expect("seek high");
    file.write_all(SENTINEL_HIGH)
        .await
        .expect("write high sentinel");

    let end_offset = FILE_SIZE - SENTINEL_END.len() as u64;
    file.seek(SeekFrom::Start(end_offset))
        .await
        .expect("seek end");
    file.write_all(SENTINEL_END)
        .await
        .expect("write end sentinel");
    file.flush().await.expect("flush");
    drop(file);

    let store = Arc::new(LocalFsImageStore::new(dir.path()));
    let metrics = Arc::new(Metrics::new());
    let state = ImagesState::new(store, metrics).with_range_options(RangeOptions {
        // Keep abuse guards low; the test only requests a few bytes.
        max_total_bytes: 1024,
    });
    let app = router_with_state(state);

    // Explicit range starting beyond 2^32.
    let high_end = HIGH_OFFSET + SENTINEL_HIGH.len() as u64 - 1;
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/large.img")
                .header(header::RANGE, format!("bytes={HIGH_OFFSET}-{high_end}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        res.headers()[header::CONTENT_RANGE].to_str().unwrap(),
        format!("bytes {HIGH_OFFSET}-{high_end}/{FILE_SIZE}")
    );
    assert_eq!(
        res.headers()[header::CONTENT_LENGTH].to_str().unwrap(),
        SENTINEL_HIGH.len().to_string()
    );
    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], SENTINEL_HIGH);

    // Suffix range: last N bytes of a file > 4GiB.
    let suffix_len = SENTINEL_END.len();
    let res = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/images/large.img")
                .header(header::RANGE, format!("bytes=-{suffix_len}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let suffix_start = FILE_SIZE - suffix_len as u64;
    let suffix_end = FILE_SIZE - 1;

    assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        res.headers()[header::CONTENT_RANGE].to_str().unwrap(),
        format!("bytes {suffix_start}-{suffix_end}/{FILE_SIZE}")
    );
    assert_eq!(
        res.headers()[header::CONTENT_LENGTH].to_str().unwrap(),
        suffix_len.to_string()
    );
    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], SENTINEL_END);
}
