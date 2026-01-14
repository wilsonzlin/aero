#![cfg(not(target_arch = "wasm32"))]

use aero_storage::{
    ChunkedStreamingDisk, ChunkedStreamingDiskConfig, ChunkedStreamingDiskError,
    StreamingCacheBackend,
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
    counters: Counters,
}

async fn start_chunked_server(
    image: Vec<u8>,
    chunk_size: u64,
    manifest_body: String,
    wrong_chunk: Option<(u64, Vec<u8>)>,
) -> (Url, Arc<State>, oneshot::Sender<()>) {
    let state = Arc::new(State {
        image: Arc::new(image),
        chunk_size,
        manifest_body,
        wrong_chunk,
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
    match *req.method() {
        Method::GET => match req.uri().path() {
            "/manifest.json" => {
                state.counters.manifest_get.fetch_add(1, Ordering::SeqCst);
                let mut resp = Response::new(Body::from(state.manifest_body.clone()));
                *resp.status_mut() = StatusCode::OK;
                resp.headers_mut().insert(
                    hyper::header::CONTENT_TYPE,
                    "application/json".parse().unwrap(),
                );
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
                return Ok(resp);
            }
            _ => {}
        },
        _ => {}
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
    let total_size = 512usize;
    let image: Vec<u8> = vec![0u8; total_size];
    let chunk_size = 512u64;
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
async fn reads_span_boundaries_and_cache_reuses_across_runs() {
    // totalSize must be a multiple of 512.
    let total_size = 4608usize; // 9 * 512
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
    let total_size = 1536usize; // 3 * 512, so final chunk is smaller
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
            { "size": 512, "sha256": expected1 },
        ]
    });
    let manifest_body = serde_json::to_string(&manifest).unwrap();

    // Serve the final chunk with the correct length but wrong contents.
    let wrong_chunk_bytes = vec![0u8; 512];
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
    let mut buf = vec![0u8; 512];
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
    let mut buf2 = vec![0u8; 512];
    let _ = disk.read_at(1024, &mut buf2).await.err().unwrap();
    assert_eq!(
        state.counters.chunk_get.load(Ordering::SeqCst),
        2,
        "expected two chunk fetches due to missing cache write on integrity failure"
    );

    let _ = shutdown.send(());
}
