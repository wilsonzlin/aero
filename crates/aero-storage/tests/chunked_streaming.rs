#![cfg(not(target_arch = "wasm32"))]

use aero_storage::{
    ChunkedStreamingDisk, ChunkedStreamingDiskConfig, ChunkedStreamingDiskError,
    StreamingCacheBackend, SECTOR_SIZE,
};
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Method, Request, Response, Server, StatusCode};
use sha2::{Digest, Sha256};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tempfile::tempdir;
use tokio::sync::oneshot;
use url::Url;

#[derive(Default)]
struct Counters {
    manifest_get: AtomicUsize,
    chunk_get: AtomicUsize,
}

struct State {
    image: Arc<Vec<u8>>,
    chunk_size: u64,
    manifest_body: String,
    wrong_chunk: Option<(u64, Vec<u8>)>,
    manifest_cache_control: Option<&'static str>,
    chunk_cache_control: Option<&'static str>,
    counters: Counters,
}

async fn start_chunked_server(
    image: Vec<u8>,
    chunk_size: u64,
    manifest_body: String,
    wrong_chunk: Option<(u64, Vec<u8>)>,
) -> (Url, Arc<State>, oneshot::Sender<()>) {
    start_chunked_server_with_cache_control(
        image,
        chunk_size,
        manifest_body,
        wrong_chunk,
        Some("no-transform"),
        Some("no-transform"),
    )
    .await
}

async fn start_chunked_server_with_cache_control(
    image: Vec<u8>,
    chunk_size: u64,
    manifest_body: String,
    wrong_chunk: Option<(u64, Vec<u8>)>,
    manifest_cache_control: Option<&'static str>,
    chunk_cache_control: Option<&'static str>,
) -> (Url, Arc<State>, oneshot::Sender<()>) {
    let state = Arc::new(State {
        image: Arc::new(image),
        chunk_size,
        manifest_body,
        wrong_chunk,
        manifest_cache_control,
        chunk_cache_control,
        counters: Counters::default(),
    });

    let make_svc = {
        let state = state.clone();
        make_service_fn(move |_conn| {
            let state = state.clone();
            async move { Ok::<_, Infallible>(service_fn(move |req| handle_request(req, state.clone()))) }
        })
    };

    let addr: SocketAddr = ([127, 0, 0, 1], 0).into();
    let builder = Server::try_bind(&addr).expect("bind");
    let local_addr = builder.local_addr();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let server = builder.serve(make_svc).with_graceful_shutdown(async move {
        let _ = shutdown_rx.await;
    });
    tokio::spawn(server);

    let url = Url::parse(&format!("http://{local_addr}/manifest.json")).expect("url");
    (url, state, shutdown_tx)
}

async fn handle_request(
    req: Request<Body>,
    state: Arc<State>,
) -> Result<Response<Body>, Infallible> {
    if *req.method() == Method::GET {
        match req.uri().path() {
            "/manifest.json" => {
                state.counters.manifest_get.fetch_add(1, Ordering::SeqCst);
                let mut resp = Response::new(Body::from(state.manifest_body.clone()));
                *resp.status_mut() = StatusCode::OK;
                resp.headers_mut().insert(
                    hyper::header::CONTENT_TYPE,
                    "application/json".parse().unwrap(),
                );
                if let Some(v) = state.manifest_cache_control {
                    resp.headers_mut()
                        .insert(hyper::header::CACHE_CONTROL, v.parse().unwrap());
                }
                return Ok(resp);
            }
            path if path.starts_with("/chunks/") && path.ends_with(".bin") => {
                state.counters.chunk_get.fetch_add(1, Ordering::SeqCst);
                let file = &path["/chunks/".len()..path.len() - ".bin".len()];
                let idx: u64 = file.trim_start_matches('0').parse().unwrap_or(0);
                if let Some((wrong_idx, wrong_bytes)) = state.wrong_chunk.as_ref() {
                    if *wrong_idx == idx {
                        let mut resp = Response::new(Body::from(wrong_bytes.clone()));
                        *resp.status_mut() = StatusCode::OK;
                        resp.headers_mut().insert(
                            hyper::header::CONTENT_TYPE,
                            "application/octet-stream".parse().unwrap(),
                        );
                        if let Some(v) = state.chunk_cache_control {
                            resp.headers_mut()
                                .insert(hyper::header::CACHE_CONTROL, v.parse().unwrap());
                        }
                        return Ok(resp);
                    }
                }

                let start = idx * state.chunk_size;
                let end = (start + state.chunk_size).min(state.image.len() as u64);
                if start >= end {
                    let mut resp = Response::new(Body::empty());
                    *resp.status_mut() = StatusCode::NOT_FOUND;
                    return Ok(resp);
                }
                let body = state.image[start as usize..end as usize].to_vec();
                let mut resp = Response::new(Body::from(body));
                *resp.status_mut() = StatusCode::OK;
                resp.headers_mut().insert(
                    hyper::header::CONTENT_TYPE,
                    "application/octet-stream".parse().unwrap(),
                );
                if let Some(v) = state.chunk_cache_control {
                    resp.headers_mut()
                        .insert(hyper::header::CACHE_CONTROL, v.parse().unwrap());
                }
                return Ok(resp);
            }
            _ => {}
        }
    }

    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = StatusCode::NOT_FOUND;
    Ok(resp)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

#[tokio::test(flavor = "current_thread")]
async fn rejects_manifests_with_unreasonably_large_chunk_index_width() {
    let total_size = SECTOR_SIZE;
    let image: Vec<u8> = vec![0u8; total_size];
    let chunk_size = SECTOR_SIZE as u64;
    let total_size_u64 = image.len() as u64;
    let chunk_count = total_size_u64.div_ceil(chunk_size);

    let manifest = serde_json::json!({
        "schema": "aero.chunked-disk-image.v1",
        "version": "bad-width",
        "mimeType": "application/octet-stream",
        "totalSize": total_size_u64,
        "chunkSize": chunk_size,
        "chunkCount": chunk_count,
        "chunkIndexWidth": 33,
    });
    let manifest_body = serde_json::to_string(&manifest).unwrap();

    let (url, _state, shutdown) =
        start_chunked_server(image.clone(), chunk_size, manifest_body, None).await;

    let cache_dir = tempdir().unwrap();
    let mut config = ChunkedStreamingDiskConfig::new(url, cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.max_retries = 1;
    config.options.max_concurrent_fetches = 1;

    let err = match ChunkedStreamingDisk::open(config).await {
        Ok(_) => panic!("expected ChunkedStreamingDisk::open to fail"),
        Err(err) => err,
    };
    match err {
        ChunkedStreamingDiskError::Protocol(msg) => {
            assert!(msg.to_ascii_lowercase().contains("chunkindexwidth"));
            assert!(msg.to_ascii_lowercase().contains("too large"));
        }
        other => panic!("expected Protocol error, got {other:?}"),
    }

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "current_thread")]
async fn rejects_manifests_without_cache_control_no_transform() {
    let total_size = SECTOR_SIZE;
    let image: Vec<u8> = vec![0u8; total_size];
    let chunk_size = SECTOR_SIZE as u64;
    let total_size_u64 = image.len() as u64;
    let chunk_count = total_size_u64.div_ceil(chunk_size);

    let manifest = serde_json::json!({
        "schema": "aero.chunked-disk-image.v1",
        "version": "missing-no-transform",
        "mimeType": "application/octet-stream",
        "totalSize": total_size_u64,
        "chunkSize": chunk_size,
        "chunkCount": chunk_count,
        "chunkIndexWidth": 8,
    });
    let manifest_body = serde_json::to_string(&manifest).unwrap();

    let (url, _state, shutdown) = start_chunked_server_with_cache_control(
        image.clone(),
        chunk_size,
        manifest_body,
        None,
        Some("public, max-age=60"),
        Some("no-transform"),
    )
    .await;

    let cache_dir = tempdir().unwrap();
    let mut config = ChunkedStreamingDiskConfig::new(url, cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.max_retries = 1;
    config.options.max_concurrent_fetches = 1;

    let err = ChunkedStreamingDisk::open(config).await.err().unwrap();
    match err {
        ChunkedStreamingDiskError::Protocol(msg) => {
            let lower = msg.to_ascii_lowercase();
            assert!(lower.contains("cache-control"), "unexpected error: {msg}");
            assert!(lower.contains("no-transform"), "unexpected error: {msg}");
        }
        other => panic!("expected Protocol error, got {other:?}"),
    }

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "current_thread")]
async fn rejects_chunks_without_cache_control_no_transform() {
    let total_size = SECTOR_SIZE;
    let image: Vec<u8> = (0..total_size).map(|i| (i % 251) as u8).collect();
    let chunk_size = SECTOR_SIZE as u64;
    let total_size_u64 = image.len() as u64;
    let chunk_count = total_size_u64.div_ceil(chunk_size);

    let manifest = serde_json::json!({
        "schema": "aero.chunked-disk-image.v1",
        "version": "chunk-missing-no-transform",
        "mimeType": "application/octet-stream",
        "totalSize": total_size_u64,
        "chunkSize": chunk_size,
        "chunkCount": chunk_count,
        "chunkIndexWidth": 8,
    });
    let manifest_body = serde_json::to_string(&manifest).unwrap();

    let (url, state, shutdown) = start_chunked_server_with_cache_control(
        image.clone(),
        chunk_size,
        manifest_body,
        None,
        Some("no-transform"),
        Some("public, max-age=60"),
    )
    .await;

    let cache_dir = tempdir().unwrap();
    let mut config = ChunkedStreamingDiskConfig::new(url, cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.max_retries = 3;
    config.options.max_concurrent_fetches = 1;

    let disk = ChunkedStreamingDisk::open(config).await.unwrap();
    let mut buf = vec![0u8; SECTOR_SIZE];
    let err = disk.read_at(0, &mut buf).await.err().unwrap();
    match err {
        ChunkedStreamingDiskError::Protocol(msg) => {
            let lower = msg.to_ascii_lowercase();
            assert!(lower.contains("cache-control"), "unexpected error: {msg}");
            assert!(lower.contains("no-transform"), "unexpected error: {msg}");
        }
        other => panic!("expected Protocol error, got {other:?}"),
    }
    assert_eq!(
        state.counters.chunk_get.load(Ordering::SeqCst),
        1,
        "protocol errors should not be retried"
    );

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "current_thread")]
async fn rejects_manifests_with_zero_total_size() {
    let image: Vec<u8> = Vec::new();
    let chunk_size = SECTOR_SIZE as u64;
    let manifest = serde_json::json!({
        "schema": "aero.chunked-disk-image.v1",
        "version": "zero-total-size",
        "mimeType": "application/octet-stream",
        "totalSize": 0u64,
        "chunkSize": chunk_size,
        "chunkCount": 0u64,
        "chunkIndexWidth": 1,
    });
    let manifest_body = serde_json::to_string(&manifest).unwrap();

    let (url, _state, shutdown) =
        start_chunked_server(image, chunk_size, manifest_body, None).await;

    let cache_dir = tempdir().unwrap();
    let mut config = ChunkedStreamingDiskConfig::new(url, cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.max_retries = 1;
    config.options.max_concurrent_fetches = 1;

    let err = match ChunkedStreamingDisk::open(config).await {
        Ok(_) => panic!("expected ChunkedStreamingDisk::open to fail"),
        Err(err) => err,
    };
    match err {
        ChunkedStreamingDiskError::Protocol(msg) => {
            assert!(msg.to_ascii_lowercase().contains("totalsize"));
            assert!(msg.to_ascii_lowercase().contains("> 0"));
        }
        other => panic!("expected Protocol error, got {other:?}"),
    }

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "current_thread")]
async fn rejects_manifests_with_too_small_chunk_index_width() {
    let total_size = SECTOR_SIZE * 11;
    let image: Vec<u8> = vec![0u8; total_size];
    let chunk_size = SECTOR_SIZE as u64;
    let total_size_u64 = image.len() as u64;
    let chunk_count = total_size_u64.div_ceil(chunk_size);

    let manifest = serde_json::json!({
        "schema": "aero.chunked-disk-image.v1",
        "version": "bad-min-width",
        "mimeType": "application/octet-stream",
        "totalSize": total_size_u64,
        "chunkSize": chunk_size,
        "chunkCount": chunk_count,
        // chunkCount=11 => last index=10 => min width=2
        "chunkIndexWidth": 1,
    });
    let manifest_body = serde_json::to_string(&manifest).unwrap();

    let (url, _state, shutdown) =
        start_chunked_server(image.clone(), chunk_size, manifest_body, None).await;

    let cache_dir = tempdir().unwrap();
    let mut config = ChunkedStreamingDiskConfig::new(url, cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.max_retries = 1;
    config.options.max_concurrent_fetches = 1;

    let err = match ChunkedStreamingDisk::open(config).await {
        Ok(_) => panic!("expected ChunkedStreamingDisk::open to fail"),
        Err(err) => err,
    };
    match err {
        ChunkedStreamingDiskError::Protocol(msg) => {
            assert!(msg.to_ascii_lowercase().contains("chunkindexwidth"));
            assert!(msg.to_ascii_lowercase().contains("too small"));
        }
        other => panic!("expected Protocol error, got {other:?}"),
    }

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "current_thread")]
async fn rejects_manifests_with_chunk_size_too_large() {
    let chunk_size = 64 * 1024 * 1024 + (SECTOR_SIZE as u64);
    let image: Vec<u8> = vec![0u8; SECTOR_SIZE];
    let manifest = serde_json::json!({
        "schema": "aero.chunked-disk-image.v1",
        "version": "big-chunk-size",
        "mimeType": "application/octet-stream",
        "totalSize": chunk_size,
        "chunkSize": chunk_size,
        "chunkCount": 1,
        "chunkIndexWidth": 8,
    });
    let manifest_body = serde_json::to_string(&manifest).unwrap();

    let (url, _state, shutdown) =
        start_chunked_server(image.clone(), chunk_size, manifest_body, None).await;

    let cache_dir = tempdir().unwrap();
    let mut config = ChunkedStreamingDiskConfig::new(url, cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.max_retries = 1;
    config.options.max_concurrent_fetches = 1;

    let err = match ChunkedStreamingDisk::open(config).await {
        Ok(_) => panic!("expected ChunkedStreamingDisk::open to fail"),
        Err(err) => err,
    };
    match err {
        ChunkedStreamingDiskError::Protocol(msg) => {
            assert!(msg.to_ascii_lowercase().contains("chunksize"));
            assert!(msg.to_ascii_lowercase().contains("exceeds"));
        }
        other => panic!("expected Protocol error, got {other:?}"),
    }

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "current_thread")]
async fn rejects_manifests_with_chunk_count_too_large() {
    let total_size = SECTOR_SIZE as u64;
    let image: Vec<u8> = vec![0u8; total_size as usize];
    let chunk_size = SECTOR_SIZE as u64;
    let manifest = serde_json::json!({
        "schema": "aero.chunked-disk-image.v1",
        "version": "big-chunk-count",
        "mimeType": "application/octet-stream",
        "totalSize": total_size,
        "chunkSize": chunk_size,
        "chunkCount": 500_001,
        "chunkIndexWidth": 8,
    });
    let manifest_body = serde_json::to_string(&manifest).unwrap();

    let (url, _state, shutdown) =
        start_chunked_server(image.clone(), chunk_size, manifest_body, None).await;

    let cache_dir = tempdir().unwrap();
    let mut config = ChunkedStreamingDiskConfig::new(url, cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.max_retries = 1;
    config.options.max_concurrent_fetches = 1;

    let err = match ChunkedStreamingDisk::open(config).await {
        Ok(_) => panic!("expected ChunkedStreamingDisk::open to fail"),
        Err(err) => err,
    };
    match err {
        ChunkedStreamingDiskError::Protocol(msg) => {
            assert!(msg.to_ascii_lowercase().contains("chunkcount"));
            assert!(msg.to_ascii_lowercase().contains("exceeds"));
        }
        other => panic!("expected Protocol error, got {other:?}"),
    }

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "current_thread")]
async fn reads_span_boundaries_and_cache_reuses_across_runs() {
    // totalSize must be a multiple of 512.
    let total_size = 9 * SECTOR_SIZE;
    let image: Vec<u8> = (0..total_size).map(|i| (i % 251) as u8).collect();
    let chunk_size = 1024u64;
    let total_size_u64 = image.len() as u64;
    let chunk_count = total_size_u64.div_ceil(chunk_size);

    // Exercise the optional `chunks` field by omitting it.
    let manifest = serde_json::json!({
        "schema": "aero.chunked-disk-image.v1",
        "version": "test-v1",
        "mimeType": "application/octet-stream",
        "totalSize": total_size_u64,
        "chunkSize": chunk_size,
        "chunkCount": chunk_count,
        "chunkIndexWidth": 8,
    });
    let manifest_body = serde_json::to_string(&manifest).unwrap();

    let (url, state, shutdown) =
        start_chunked_server(image.clone(), chunk_size, manifest_body, None).await;

    let cache_dir = tempdir().unwrap();
    let mut config = ChunkedStreamingDiskConfig::new(url.clone(), cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.max_retries = 1;
    config.options.max_concurrent_fetches = 4;

    let disk = ChunkedStreamingDisk::open(config.clone()).await.unwrap();
    assert_eq!(disk.capacity_bytes() as usize, image.len());

    let mut buf = vec![0u8; 200];
    disk.read_at(1000, &mut buf).await.unwrap();
    assert_eq!(&buf[..], &image[1000..1200]);
    assert_eq!(
        state.counters.chunk_get.load(Ordering::SeqCst),
        2,
        "read should fetch chunks 0 and 1"
    );

    let mut buf2 = vec![0u8; 200];
    disk.read_at(1000, &mut buf2).await.unwrap();
    assert_eq!(&buf2[..], &image[1000..1200]);
    assert_eq!(
        state.counters.chunk_get.load(Ordering::SeqCst),
        2,
        "second read should hit cache"
    );
    drop(disk);

    // Re-open with a different query string to verify the persistent cache is keyed by manifest
    // content/version, not the raw URL (signed URL tokens should not break reuse).
    let mut url2 = url.clone();
    url2.set_query(Some("token=ignored"));
    let mut config2 = ChunkedStreamingDiskConfig::new(url2, cache_dir.path());
    config2.cache_backend = StreamingCacheBackend::Directory;
    config2.options.max_retries = 1;
    config2.options.max_concurrent_fetches = 4;

    let disk2 = ChunkedStreamingDisk::open(config2).await.unwrap();
    let mut buf3 = vec![0u8; 200];
    disk2.read_at(1000, &mut buf3).await.unwrap();
    assert_eq!(&buf3[..], &image[1000..1200]);
    assert_eq!(
        state.counters.chunk_get.load(Ordering::SeqCst),
        2,
        "persistent cache should be reused across opens"
    );
    assert_eq!(
        state.counters.manifest_get.load(Ordering::SeqCst),
        2,
        "manifest should be fetched on each open"
    );

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "current_thread")]
async fn sha256_mismatch_is_deterministic_error() {
    let total_size = 3 * SECTOR_SIZE; // final chunk is smaller
    let image: Vec<u8> = (0..total_size).map(|i| (i % 251) as u8).collect();
    let chunk_size = 1024u64;
    let total_size_u64 = image.len() as u64;
    let chunk_count = total_size_u64.div_ceil(chunk_size);
    assert_eq!(chunk_count, 2);

    let chunk0 = &image[0..1024];
    let chunk1 = &image[1024..1536];
    let expected0 = sha256_hex(chunk0);
    let expected1 = sha256_hex(chunk1);

    let manifest = serde_json::json!({
        "schema": "aero.chunked-disk-image.v1",
        "version": "sha-test",
        "mimeType": "application/octet-stream",
        "totalSize": total_size_u64,
        "chunkSize": chunk_size,
        "chunkCount": chunk_count,
        "chunkIndexWidth": 8,
        "chunks": [
            { "size": 1024, "sha256": expected0 },
            { "size": SECTOR_SIZE, "sha256": expected1 },
        ]
    });
    let manifest_body = serde_json::to_string(&manifest).unwrap();

    // Serve the final chunk with the correct length but wrong contents.
    let wrong_chunk_bytes = vec![0u8; SECTOR_SIZE];
    let actual1 = sha256_hex(&wrong_chunk_bytes);

    let (url, state, shutdown) = start_chunked_server(
        image.clone(),
        chunk_size,
        manifest_body,
        Some((1, wrong_chunk_bytes)),
    )
    .await;

    let cache_dir = tempdir().unwrap();
    let mut config = ChunkedStreamingDiskConfig::new(url, cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.max_retries = 1;

    let disk = ChunkedStreamingDisk::open(config).await.unwrap();
    let mut buf = vec![0u8; SECTOR_SIZE];
    let err = disk.read_at(1024, &mut buf).await.err().unwrap();
    match err {
        ChunkedStreamingDiskError::Integrity {
            chunk_index,
            expected,
            actual,
        } => {
            assert_eq!(chunk_index, 1);
            assert_eq!(expected, expected1);
            assert_eq!(actual, actual1);
        }
        other => panic!("expected Integrity error, got {other:?}"),
    }

    // Integrity failures should not populate the cache; a second attempt should hit the server
    // again rather than reading a poisoned cached chunk.
    let mut buf2 = vec![0u8; SECTOR_SIZE];
    let _ = disk.read_at(1024, &mut buf2).await.err().unwrap();
    assert_eq!(
        state.counters.chunk_get.load(Ordering::SeqCst),
        2,
        "expected two chunk fetches due to missing cache write on integrity failure"
    );

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "current_thread")]
async fn http_errors_redact_url_query() {
    // Create a URL that should fail to connect (unused local port), but embeds a query token.
    // The returned error message should not leak the query string.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let url = Url::parse(&format!(
        "http://127.0.0.1:{port}/manifest.json?token=supersecret"
    ))
    .unwrap();
    let cache_dir = tempdir().unwrap();
    let config = ChunkedStreamingDiskConfig::new(url, cache_dir.path());
    let err = ChunkedStreamingDisk::open(config).await.err().unwrap();
    let ChunkedStreamingDiskError::Http(msg) = err else {
        panic!("expected Http error, got {err:?}");
    };
    assert!(
        !msg.contains("token=supersecret"),
        "error message should not contain query tokens: {msg}"
    );
}
