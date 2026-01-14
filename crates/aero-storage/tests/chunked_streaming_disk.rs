#![cfg(not(target_arch = "wasm32"))]

use aero_storage::{
    ChunkedStreamingDiskConfig, ChunkedStreamingDiskError, ChunkedStreamingDiskSync,
    StreamingCacheBackend, SECTOR_SIZE,
};
use hyper::header::CACHE_CONTROL;
use hyper::header::CONTENT_LENGTH;
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
    chunk_size: usize,
    chunk_count: u64,
    manifest_body: String,
    corrupt_chunk: Option<u64>,
    counters: Counters,
}

async fn start_chunk_server(
    image: Vec<u8>,
    chunk_size: usize,
    manifest_body: String,
    corrupt_chunk: Option<u64>,
) -> (Url, Arc<State>, oneshot::Sender<()>) {
    let total_size = image.len() as u64;
    let chunk_count = total_size.div_ceil(chunk_size as u64);
    let state = Arc::new(State {
        image: Arc::new(image),
        chunk_size,
        chunk_count,
        manifest_body,
        corrupt_chunk,
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
        Method::GET => {}
        _ => {
            let mut resp = Response::new(Body::empty());
            *resp.status_mut() = StatusCode::METHOD_NOT_ALLOWED;
            return Ok(resp);
        }
    }

    let path = req.uri().path();
    if path == "/manifest.json" {
        state.counters.manifest_get.fetch_add(1, Ordering::SeqCst);
        let mut resp = Response::new(Body::from(state.manifest_body.clone()));
        *resp.status_mut() = StatusCode::OK;
        resp.headers_mut()
            .insert(CACHE_CONTROL, "no-transform".parse().unwrap());
        resp.headers_mut().insert(
            CONTENT_LENGTH,
            (state.manifest_body.len() as u64)
                .to_string()
                .parse()
                .unwrap(),
        );
        return Ok(resp);
    }

    if let Some(rest) = path.strip_prefix("/chunks/") {
        state.counters.chunk_get.fetch_add(1, Ordering::SeqCst);
        let Some(name) = rest.strip_suffix(".bin") else {
            let mut resp = Response::new(Body::empty());
            *resp.status_mut() = StatusCode::NOT_FOUND;
            return Ok(resp);
        };
        let chunk_index: u64 = match name.parse() {
            Ok(v) => v,
            Err(_) => {
                let mut resp = Response::new(Body::empty());
                *resp.status_mut() = StatusCode::NOT_FOUND;
                return Ok(resp);
            }
        };
        if chunk_index >= state.chunk_count {
            let mut resp = Response::new(Body::empty());
            *resp.status_mut() = StatusCode::NOT_FOUND;
            return Ok(resp);
        }

        let start = chunk_index * (state.chunk_size as u64);
        let end = (start + state.chunk_size as u64).min(state.image.len() as u64);
        let mut bytes = state.image[start as usize..end as usize].to_vec();
        if state.corrupt_chunk == Some(chunk_index) && !bytes.is_empty() {
            bytes[0] ^= 0xFF;
        }

        let mut resp = Response::new(Body::from(bytes.clone()));
        *resp.status_mut() = StatusCode::OK;
        resp.headers_mut()
            .insert(CACHE_CONTROL, "no-transform".parse().unwrap());
        resp.headers_mut().insert(
            CONTENT_LENGTH,
            (bytes.len() as u64).to_string().parse().unwrap(),
        );
        return Ok(resp);
    }

    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = StatusCode::NOT_FOUND;
    Ok(resp)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = [0u8; 64];
    const LUT: &[u8; 16] = b"0123456789abcdef";
    for (i, b) in digest.iter().copied().enumerate() {
        out[i * 2] = LUT[(b >> 4) as usize];
        out[i * 2 + 1] = LUT[(b & 0xF) as usize];
    }
    // Safety: LUT is valid UTF-8.
    unsafe { String::from_utf8_unchecked(out.to_vec()) }
}

fn build_manifest(image: &[u8], chunk_size: usize, version: &str) -> String {
    let total_size = image.len() as u64;
    let chunk_size_u64 = chunk_size as u64;
    let chunk_count = total_size.div_ceil(chunk_size_u64);

    let mut chunks = Vec::new();
    for chunk_index in 0..chunk_count {
        let start = (chunk_index * chunk_size_u64) as usize;
        let end = ((chunk_index + 1) * chunk_size_u64).min(total_size) as usize;
        let bytes = &image[start..end];
        chunks.push(serde_json::json!({
            "size": bytes.len() as u64,
            "sha256": sha256_hex(bytes),
        }));
    }

    serde_json::json!({
        "schema": "aero.chunked-disk-image.v1",
        "version": version,
        "mimeType": "application/octet-stream",
        "totalSize": total_size,
        "chunkSize": chunk_size_u64,
        "chunkCount": chunk_count,
        "chunkIndexWidth": 8,
        "chunks": chunks,
    })
    .to_string()
}

#[tokio::test(flavor = "current_thread")]
async fn chunked_streaming_reads_and_reuses_cache() {
    // totalSize must be sector-aligned per the manifest spec/parser.
    let image: Vec<u8> = (0..(4096 + SECTOR_SIZE)).map(|i| (i % 251) as u8).collect();
    let chunk_size = 1024usize;
    let manifest_body = build_manifest(&image, chunk_size, "v1");
    let (url, state, shutdown) =
        start_chunk_server(image.clone(), chunk_size, manifest_body, None).await;

    let cache_dir = tempdir().unwrap();
    let cache_path = cache_dir.path().to_path_buf();
    let url2 = url.clone();
    let state2 = state.clone();

    let (buf1, buf2, buf3, after_first, after_second, after_reopen) =
        tokio::task::spawn_blocking(move || {
            let mut config = ChunkedStreamingDiskConfig::new(url, cache_path.clone());
            config.cache_backend = StreamingCacheBackend::Directory;
            let mut disk = ChunkedStreamingDiskSync::open(config).unwrap();

            let mut buf1 = vec![0u8; 200];
            disk.read_at(1000, &mut buf1).unwrap();
            let after_first = state2.counters.chunk_get.load(Ordering::SeqCst);

            let mut buf2 = vec![0u8; 200];
            disk.read_at(1000, &mut buf2).unwrap();
            let after_second = state2.counters.chunk_get.load(Ordering::SeqCst);

            drop(disk);

            let mut config2 = ChunkedStreamingDiskConfig::new(url2, cache_path);
            config2.cache_backend = StreamingCacheBackend::Directory;
            let mut disk2 = ChunkedStreamingDiskSync::open(config2).unwrap();

            let mut buf3 = vec![0u8; 200];
            disk2.read_at(1000, &mut buf3).unwrap();
            let after_reopen = state2.counters.chunk_get.load(Ordering::SeqCst);

            (buf1, buf2, buf3, after_first, after_second, after_reopen)
        })
        .await
        .unwrap();

    assert_eq!(&buf1[..], &image[1000..1200]);
    assert_eq!(&buf2[..], &image[1000..1200]);
    assert_eq!(&buf3[..], &image[1000..1200]);

    assert_eq!(after_first, 2);
    assert_eq!(after_second, 2, "second read should hit cache");
    assert_eq!(after_reopen, 2, "cache should persist across re-open");

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "current_thread")]
async fn chunked_streaming_detects_sha256_mismatch() {
    let image: Vec<u8> = (0..2048).map(|i| (i % 251) as u8).collect();
    let chunk_size = 1024usize;
    let manifest_body = build_manifest(&image, chunk_size, "v-integrity");
    let (url, state, shutdown) =
        start_chunk_server(image.clone(), chunk_size, manifest_body, Some(0)).await;

    let cache_dir = tempdir().unwrap();
    let cache_path = cache_dir.path().to_path_buf();
    let state2 = state.clone();

    let (err, chunk_gets) = tokio::task::spawn_blocking(move || {
        let mut config = ChunkedStreamingDiskConfig::new(url, cache_path);
        config.cache_backend = StreamingCacheBackend::Directory;
        let mut disk = ChunkedStreamingDiskSync::open(config).unwrap();

        let mut buf = vec![0u8; 16];
        let err = disk.read_at(0, &mut buf).err().unwrap();
        let chunk_gets = state2.counters.chunk_get.load(Ordering::SeqCst);
        (err, chunk_gets)
    })
    .await
    .unwrap();

    assert!(matches!(err, ChunkedStreamingDiskError::Integrity { .. }));
    assert_eq!(chunk_gets, 1);

    let _ = shutdown.send(());
}
