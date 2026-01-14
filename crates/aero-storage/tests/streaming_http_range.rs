#![cfg(not(target_arch = "wasm32"))]

use aero_storage::{
    ChunkManifest, StreamingCacheBackend, StreamingDisk, StreamingDiskConfig, StreamingDiskError,
};
use hyper::header::{
    ACCEPT_RANGES, CACHE_CONTROL, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_RANGE, IF_RANGE,
    LAST_MODIFIED, RANGE,
};
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Method, Request, Response, Server, StatusCode};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tempfile::tempdir;
use tokio::sync::oneshot;
use url::Url;

#[derive(Default)]
struct Counters {
    head: AtomicUsize,
    get_range: AtomicUsize,
    get_full: AtomicUsize,
}

struct State {
    image: Arc<Vec<u8>>,
    etag: std::sync::Mutex<String>,
    fail_first_range: AtomicBool,
    required_header: Option<(String, String)>,
    head_accept_ranges: bool,
    ignore_range: bool,
    enforce_strong_if_range: bool,
    wrong_content_range: bool,
    content_range_total_star: bool,
    content_encoding: Option<String>,
    counters: Counters,
}

struct LastModifiedState {
    image: Arc<Vec<u8>>,
    last_modified: String,
    counters: Counters,
}

#[derive(Clone, Copy)]
struct RangeServerOptions<'a> {
    etag: &'a str,
    fail_first_range: bool,
    required_header: Option<(&'a str, &'a str)>,
    head_accept_ranges: bool,
    ignore_range: bool,
    enforce_strong_if_range: bool,
    wrong_content_range: bool,
    content_range_total_star: bool,
    content_encoding: Option<&'a str>,
}

impl<'a> RangeServerOptions<'a> {
    fn new(etag: &'a str) -> Self {
        Self {
            etag,
            fail_first_range: false,
            required_header: None,
            head_accept_ranges: true,
            ignore_range: false,
            enforce_strong_if_range: false,
            wrong_content_range: false,
            content_range_total_star: false,
            content_encoding: None,
        }
    }
}

async fn start_range_server_with_options(
    image: Vec<u8>,
    options: RangeServerOptions<'_>,
) -> (Url, Arc<State>, oneshot::Sender<()>) {
    let state = Arc::new(State {
        image: Arc::new(image),
        etag: std::sync::Mutex::new(options.etag.to_string()),
        fail_first_range: AtomicBool::new(options.fail_first_range),
        required_header: options
            .required_header
            .map(|(k, v)| (k.to_string(), v.to_string())),
        head_accept_ranges: options.head_accept_ranges,
        ignore_range: options.ignore_range,
        enforce_strong_if_range: options.enforce_strong_if_range,
        wrong_content_range: options.wrong_content_range,
        content_range_total_star: options.content_range_total_star,
        content_encoding: options.content_encoding.map(|v| v.to_string()),
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

    let url = Url::parse(&format!("http://{local_addr}/image.raw")).expect("url");
    (url, state, shutdown_tx)
}

async fn handle_request(
    req: Request<Body>,
    state: Arc<State>,
) -> Result<Response<Body>, Infallible> {
    if let Some((name, expected)) = state.required_header.as_ref() {
        let actual = req
            .headers()
            .get(name.as_str())
            .and_then(|v| v.to_str().ok());
        if actual != Some(expected.as_str()) {
            let mut resp = Response::new(Body::empty());
            *resp.status_mut() = StatusCode::UNAUTHORIZED;
            return Ok(resp);
        }
    }

    match *req.method() {
        Method::HEAD => {
            state.counters.head.fetch_add(1, Ordering::SeqCst);
            let mut resp = Response::new(Body::empty());
            *resp.status_mut() = StatusCode::OK;
            resp.headers_mut().insert(
                CONTENT_LENGTH,
                (state.image.len() as u64).to_string().parse().unwrap(),
            );
            if state.head_accept_ranges {
                resp.headers_mut()
                    .insert(ACCEPT_RANGES, "bytes".parse().unwrap());
            }
            resp.headers_mut().insert(
                hyper::header::ETAG,
                state.etag.lock().unwrap().parse().unwrap(),
            );
            return Ok(resp);
        }
        Method::GET => {
            if let Some(range_header) = req.headers().get(RANGE).and_then(|v| v.to_str().ok()) {
                state.counters.get_range.fetch_add(1, Ordering::SeqCst);

                let current_etag = state.etag.lock().unwrap().clone();
                if let Some(if_range) = req.headers().get(IF_RANGE).and_then(|v| v.to_str().ok()) {
                    let is_mismatch = if state.enforce_strong_if_range
                        && (if_range.trim_start().starts_with("W/")
                            || current_etag.trim_start().starts_with("W/"))
                    {
                        // RFC 9110 requires strong comparison and disallows weak validators in
                        // `If-Range`. Treat weak validators as not matching.
                        true
                    } else {
                        if_range != current_etag
                    };

                    if is_mismatch {
                        // Simulate RFC 7233 `If-Range` mismatch behavior: ignore the Range and
                        // return the full representation with a 200.
                        let mut resp = Response::new(Body::from(state.image.as_ref().clone()));
                        *resp.status_mut() = StatusCode::OK;
                        resp.headers_mut().insert(
                            CONTENT_LENGTH,
                            (state.image.len() as u64).to_string().parse().unwrap(),
                        );
                        resp.headers_mut()
                            .insert(ACCEPT_RANGES, "bytes".parse().unwrap());
                        resp.headers_mut()
                            .insert(hyper::header::ETAG, current_etag.parse().unwrap());
                        return Ok(resp);
                    }
                }

                if state.ignore_range {
                    let mut resp = Response::new(Body::from(state.image.as_ref().clone()));
                    *resp.status_mut() = StatusCode::OK;
                    resp.headers_mut().insert(
                        CONTENT_LENGTH,
                        (state.image.len() as u64).to_string().parse().unwrap(),
                    );
                    resp.headers_mut()
                        .insert(ACCEPT_RANGES, "bytes".parse().unwrap());
                    resp.headers_mut()
                        .insert(hyper::header::ETAG, current_etag.parse().unwrap());
                    return Ok(resp);
                }

                if state.fail_first_range.swap(false, Ordering::SeqCst) {
                    let mut resp = Response::new(Body::empty());
                    *resp.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
                    return Ok(resp);
                }

                match parse_range_header(range_header, state.image.len() as u64) {
                    Ok((start, end_exclusive)) => {
                        let end_inclusive = end_exclusive - 1;
                        let body = state.image[start as usize..end_exclusive as usize].to_vec();
                        let mut resp = Response::new(Body::from(body));
                        *resp.status_mut() = StatusCode::PARTIAL_CONTENT;
                        resp.headers_mut().insert(
                            CONTENT_LENGTH,
                            (end_exclusive - start).to_string().parse().unwrap(),
                        );
                        resp.headers_mut()
                            .insert(CACHE_CONTROL, "no-transform".parse().unwrap());
                        resp.headers_mut()
                            .insert(ACCEPT_RANGES, "bytes".parse().unwrap());
                        resp.headers_mut()
                            .insert(hyper::header::ETAG, current_etag.parse().unwrap());
                        if let Some(encoding) = state.content_encoding.as_ref() {
                            resp.headers_mut()
                                .insert(CONTENT_ENCODING, encoding.parse().unwrap());
                        }
                        let total = if state.content_range_total_star {
                            "*".to_string()
                        } else {
                            state.image.len().to_string()
                        };
                        let content_range = if state.wrong_content_range {
                            format!("bytes {}-{end_inclusive}/{total}", start + 1)
                        } else {
                            format!("bytes {start}-{end_inclusive}/{total}")
                        };
                        resp.headers_mut()
                            .insert(CONTENT_RANGE, content_range.parse().unwrap());
                        return Ok(resp);
                    }
                    Err(status) => {
                        let mut resp = Response::new(Body::empty());
                        *resp.status_mut() = status;
                        return Ok(resp);
                    }
                }
            }

            state.counters.get_full.fetch_add(1, Ordering::SeqCst);
            let mut resp = Response::new(Body::from(state.image.as_ref().clone()));
            *resp.status_mut() = StatusCode::OK;
            resp.headers_mut().insert(
                CONTENT_LENGTH,
                (state.image.len() as u64).to_string().parse().unwrap(),
            );
            resp.headers_mut()
                .insert(ACCEPT_RANGES, "bytes".parse().unwrap());
            resp.headers_mut().insert(
                hyper::header::ETAG,
                state.etag.lock().unwrap().parse().unwrap(),
            );
            return Ok(resp);
        }
        _ => {}
    }

    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = StatusCode::METHOD_NOT_ALLOWED;
    Ok(resp)
}

async fn start_last_modified_server(
    image: Vec<u8>,
    last_modified: &str,
) -> (Url, Arc<LastModifiedState>, oneshot::Sender<()>) {
    let state = Arc::new(LastModifiedState {
        image: Arc::new(image),
        last_modified: last_modified.to_string(),
        counters: Counters::default(),
    });

    let make_svc = {
        let state = state.clone();
        make_service_fn(move |_conn| {
            let state = state.clone();
            async move {
                Ok::<_, Infallible>(service_fn(move |req| {
                    let state = state.clone();
                    async move {
                        match *req.method() {
                            Method::HEAD => {
                                state.counters.head.fetch_add(1, Ordering::SeqCst);
                                let mut resp = Response::new(Body::empty());
                                *resp.status_mut() = StatusCode::OK;
                                resp.headers_mut().insert(
                                    CONTENT_LENGTH,
                                    (state.image.len() as u64).to_string().parse().unwrap(),
                                );
                                resp.headers_mut()
                                    .insert(ACCEPT_RANGES, "bytes".parse().unwrap());
                                resp.headers_mut()
                                    .insert(LAST_MODIFIED, state.last_modified.parse().unwrap());
                                Ok::<_, Infallible>(resp)
                            }
                            Method::GET => {
                                let Some(range_header) =
                                    req.headers().get(RANGE).and_then(|v| v.to_str().ok())
                                else {
                                    let mut resp = Response::new(Body::empty());
                                    *resp.status_mut() = StatusCode::BAD_REQUEST;
                                    return Ok(resp);
                                };
                                state.counters.get_range.fetch_add(1, Ordering::SeqCst);
                                let (start, end_exclusive) = match parse_range_header(
                                    range_header,
                                    state.image.len() as u64,
                                ) {
                                    Ok(v) => v,
                                    Err(status) => {
                                        let mut resp = Response::new(Body::empty());
                                        *resp.status_mut() = status;
                                        return Ok(resp);
                                    }
                                };

                                let end_inclusive = end_exclusive - 1;
                                let body =
                                    state.image[start as usize..end_exclusive as usize].to_vec();
                                let mut resp = Response::new(Body::from(body));
                                *resp.status_mut() = StatusCode::PARTIAL_CONTENT;
                                resp.headers_mut().insert(
                                    CONTENT_LENGTH,
                                    (end_exclusive - start).to_string().parse().unwrap(),
                                );
                                resp.headers_mut()
                                    .insert(CACHE_CONTROL, "no-transform".parse().unwrap());
                                resp.headers_mut()
                                    .insert(ACCEPT_RANGES, "bytes".parse().unwrap());
                                resp.headers_mut()
                                    .insert(LAST_MODIFIED, state.last_modified.parse().unwrap());
                                resp.headers_mut().insert(
                                    CONTENT_RANGE,
                                    format!("bytes {start}-{end_inclusive}/{}", state.image.len())
                                        .parse()
                                        .unwrap(),
                                );
                                Ok(resp)
                            }
                            _ => {
                                let mut resp = Response::new(Body::empty());
                                *resp.status_mut() = StatusCode::METHOD_NOT_ALLOWED;
                                Ok(resp)
                            }
                        }
                    }
                }))
            }
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

    let url = Url::parse(&format!("http://{local_addr}/image.raw")).expect("url");
    (url, state, shutdown_tx)
}

fn parse_range_header(header: &str, total_size: u64) -> Result<(u64, u64), StatusCode> {
    // Only supports a single range: bytes=start-end
    let header = header.trim();
    let Some(spec) = header.strip_prefix("bytes=") else {
        return Err(StatusCode::BAD_REQUEST);
    };
    let mut parts = spec.split('-');
    let start: u64 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or(StatusCode::BAD_REQUEST)?;
    let end_inclusive: u64 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or(StatusCode::BAD_REQUEST)?;
    if parts.next().is_some() {
        return Err(StatusCode::BAD_REQUEST);
    }
    if start >= total_size {
        return Err(StatusCode::RANGE_NOT_SATISFIABLE);
    }
    let end_exclusive = (end_inclusive + 1).min(total_size);
    if end_exclusive <= start {
        return Err(StatusCode::RANGE_NOT_SATISFIABLE);
    }
    Ok((start, end_exclusive))
}

#[tokio::test(flavor = "current_thread")]
async fn streaming_reads_and_reuses_cache() {
    let image: Vec<u8> = (0..(4096 + 123)).map(|i| (i % 251) as u8).collect();
    let (url, state, shutdown) =
        start_range_server_with_options(image.clone(), RangeServerOptions::new("etag-v1")).await;

    let cache_dir = tempdir().unwrap();
    let mut config = StreamingDiskConfig::new(url.clone(), cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.chunk_size = 1024;
    config.options.read_ahead_chunks = 0;

    let disk = StreamingDisk::open(config.clone()).await.unwrap();
    assert_eq!(disk.total_size() as usize, image.len());

    let mut buf = vec![0u8; 200];
    disk.read_at(1000, &mut buf).await.unwrap();
    assert_eq!(&buf[..], &image[1000..1200]);

    // Offset 1000..1200 touches chunks 0 and 1, so we expect 2 range GETs.
    // (Requests are chunk-aligned; i.e. two 1KiB range GETs.)
    assert_eq!(state.counters.get_range.load(Ordering::SeqCst), 2);

    let mut buf2 = vec![0u8; 200];
    disk.read_at(1000, &mut buf2).await.unwrap();
    assert_eq!(&buf2[..], &image[1000..1200]);
    assert_eq!(
        state.counters.get_range.load(Ordering::SeqCst),
        2,
        "second read should be served from cache"
    );
    let telemetry = disk.telemetry_snapshot();
    assert_eq!(telemetry.range_requests, 2);
    assert_eq!(telemetry.bytes_downloaded, 2 * 1024);
    assert_eq!(telemetry.cache_miss_chunks, 2);
    assert_eq!(telemetry.cache_hit_chunks, 2);

    drop(disk);

    // Re-open with the same cache directory; should still avoid extra range GETs.
    let mut url2 = url.clone();
    url2.set_query(Some("token=ignored"));
    let mut config2 = config;
    config2.url = url2;
    let disk2 = StreamingDisk::open(config2).await.unwrap();
    let mut buf3 = vec![0u8; 200];
    disk2.read_at(1000, &mut buf3).await.unwrap();
    assert_eq!(&buf3[..], &image[1000..1200]);
    assert_eq!(
        state.counters.get_range.load(Ordering::SeqCst),
        2,
        "cache should persist across runs (cache identity is size+validator, not URL)"
    );

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "current_thread")]
async fn cache_invalidates_on_validator_change() {
    let image: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
    let (url, state1, shutdown1) =
        start_range_server_with_options(image.clone(), RangeServerOptions::new("etag-v1")).await;

    let cache_dir = tempdir().unwrap();
    let mut config = StreamingDiskConfig::new(url, cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.chunk_size = 1024;
    config.options.read_ahead_chunks = 0;

    let disk = StreamingDisk::open(config).await.unwrap();

    let mut buf = vec![0u8; 16];
    disk.read_at(0, &mut buf).await.unwrap();
    assert_eq!(&buf[..], &image[0..16]);

    disk.read_at(1024, &mut buf).await.unwrap();
    assert_eq!(&buf[..], &image[1024..1040]);

    // Two different chunks were read, so at least 2 range GETs.
    assert_eq!(state1.counters.get_range.load(Ordering::SeqCst), 2);

    drop(disk);
    let _ = shutdown1.send(());

    // Same bytes endpoint, but validator changed => invalidate cache and re-fetch.
    let (url2, state2, shutdown2) =
        start_range_server_with_options(image.clone(), RangeServerOptions::new("etag-v2")).await;
    let mut config2 = StreamingDiskConfig::new(url2, cache_dir.path());
    config2.cache_backend = StreamingCacheBackend::Directory;
    config2.options.chunk_size = 1024;
    config2.options.read_ahead_chunks = 0;

    let disk2 = StreamingDisk::open(config2).await.unwrap();
    buf.fill(0);
    disk2.read_at(0, &mut buf).await.unwrap();
    assert_eq!(&buf[..], &image[0..16]);
    assert_eq!(
        state2.counters.get_range.load(Ordering::SeqCst),
        1,
        "cache should be invalidated and chunk re-fetched"
    );

    let _ = shutdown2.send(());
}

#[tokio::test(flavor = "current_thread")]
async fn inflight_dedup_avoids_duplicate_fetches() {
    let image: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
    let (url, state, shutdown) =
        start_range_server_with_options(image.clone(), RangeServerOptions::new("etag-dedup")).await;

    let cache_dir = tempdir().unwrap();
    let mut config = StreamingDiskConfig::new(url, cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.chunk_size = 1024;
    config.options.read_ahead_chunks = 0;
    config.options.max_retries = 2;

    let disk = StreamingDisk::open(config).await.unwrap();
    let a = {
        let disk = disk.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 128];
            disk.read_at(0, &mut buf).await.unwrap();
            buf
        })
    };
    let b = {
        let disk = disk.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 128];
            disk.read_at(0, &mut buf).await.unwrap();
            buf
        })
    };

    let (buf_a, buf_b) = tokio::join!(a, b);
    let buf_a = buf_a.unwrap();
    let buf_b = buf_b.unwrap();
    assert_eq!(&buf_a[..], &image[0..buf_a.len()]);
    assert_eq!(&buf_b[..], &image[0..buf_b.len()]);

    assert_eq!(
        state.counters.get_range.load(Ordering::SeqCst),
        1,
        "concurrent reads of the same chunk should be deduplicated"
    );
    let telemetry = disk.telemetry_snapshot();
    assert_eq!(telemetry.range_requests, 1);
    assert_eq!(telemetry.bytes_downloaded, 1024);
    assert_eq!(telemetry.cache_miss_chunks, 1);
    assert_eq!(telemetry.cache_hit_chunks, 0);

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "current_thread")]
async fn retries_transient_http_errors() {
    let image: Vec<u8> = (0..2048).map(|i| (i % 251) as u8).collect();
    let (url, state, shutdown) = start_range_server_with_options(
        image.clone(),
        RangeServerOptions {
            fail_first_range: true,
            ..RangeServerOptions::new("etag-retry")
        },
    )
    .await;

    let cache_dir = tempdir().unwrap();
    let mut config = StreamingDiskConfig::new(url, cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.chunk_size = 1024;
    config.options.read_ahead_chunks = 0;
    config.options.max_retries = 2;

    let disk = StreamingDisk::open(config).await.unwrap();
    let mut buf = vec![0u8; 16];
    disk.read_at(0, &mut buf).await.unwrap();
    assert_eq!(&buf[..], &image[0..16]);
    assert_eq!(
        state.counters.get_range.load(Ordering::SeqCst),
        2,
        "first range request fails, second succeeds"
    );

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "current_thread")]
async fn integrity_manifest_rejects_corrupt_chunk() {
    use sha2::{Digest, Sha256};

    let image: Vec<u8> = (0..2048).map(|i| (i % 251) as u8).collect();
    let (url, state, shutdown) =
        start_range_server_with_options(image.clone(), RangeServerOptions::new("etag-integrity"))
            .await;

    let chunk_size = 1024usize;
    let mut sha256 = Vec::new();
    for chunk in 0..2 {
        let start = chunk * chunk_size;
        let end = (start + chunk_size).min(image.len());
        let digest = Sha256::digest(&image[start..end]);
        let mut out = [0u8; 32];
        out.copy_from_slice(&digest);
        sha256.push(out);
    }
    sha256[0][0] ^= 0xFF;

    let cache_dir = tempdir().unwrap();
    let mut config = StreamingDiskConfig::new(url, cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.chunk_size = chunk_size as u64;
    config.options.read_ahead_chunks = 0;
    config.options.max_retries = 1;
    config.options.manifest = Some(ChunkManifest {
        chunk_size: chunk_size as u64,
        sha256,
    });

    let disk = StreamingDisk::open(config).await.unwrap();
    let mut buf = vec![0u8; 16];
    let err = disk.read_at(0, &mut buf).await.err().unwrap();
    assert!(matches!(err, StreamingDiskError::Integrity { .. }));
    assert_eq!(state.counters.get_range.load(Ordering::SeqCst), 1);

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "current_thread")]
async fn if_range_mismatch_is_reported_as_validator_mismatch() {
    let image: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
    let (url, state, shutdown) =
        start_range_server_with_options(image.clone(), RangeServerOptions::new("etag-v1")).await;

    let cache_dir = tempdir().unwrap();
    let mut config = StreamingDiskConfig::new(url, cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.chunk_size = 1024;
    config.options.read_ahead_chunks = 0;

    let disk = StreamingDisk::open(config).await.unwrap();

    // Simulate the remote changing while the disk is open.
    *state.etag.lock().unwrap() = "etag-v2".to_string();

    let mut buf = vec![0u8; 16];
    let err = disk.read_at(0, &mut buf).await.err().unwrap();
    assert!(matches!(err, StreamingDiskError::ValidatorMismatch { .. }));

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "current_thread")]
async fn request_headers_are_sent_on_all_http_requests() {
    let image: Vec<u8> = (0..2048).map(|i| (i % 251) as u8).collect();
    let (url, state, shutdown) = start_range_server_with_options(
        image.clone(),
        RangeServerOptions {
            required_header: Some(("x-test-auth", "secret")),
            ..RangeServerOptions::new("etag-auth")
        },
    )
    .await;

    let cache_dir = tempdir().unwrap();
    let mut config = StreamingDiskConfig::new(url, cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.chunk_size = 1024;
    config.options.read_ahead_chunks = 0;
    config.request_headers = vec![("X-Test-Auth".to_string(), "secret".to_string())];

    let disk = StreamingDisk::open(config).await.unwrap();
    let mut buf = vec![0u8; 16];
    disk.read_at(0, &mut buf).await.unwrap();
    assert_eq!(&buf[..], &image[0..16]);
    assert_eq!(state.counters.head.load(Ordering::SeqCst), 1);

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "current_thread")]
async fn reject_absurd_chunk_size() {
    let image: Vec<u8> = (0..2048).map(|i| (i % 251) as u8).collect();
    let (url, _state, shutdown) =
        start_range_server_with_options(image, RangeServerOptions::new("etag-chunk-size")).await;

    let cache_dir = tempdir().unwrap();
    let mut config = StreamingDiskConfig::new(url, cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    // `StreamingDisk` should reject very large chunk sizes up-front to avoid huge
    // allocations for a single Range fetch.
    config.options.chunk_size = 128 * 1024 * 1024; // 128 MiB
    config.options.read_ahead_chunks = 0;

    let err = StreamingDisk::open(config)
        .await
        .err()
        .expect("expected error");
    assert!(matches!(err, StreamingDiskError::Protocol(_)));

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "current_thread")]
async fn reject_zero_max_concurrent_fetches() {
    let image: Vec<u8> = (0..2048).map(|i| (i % 251) as u8).collect();
    let (url, _state, shutdown) =
        start_range_server_with_options(image, RangeServerOptions::new("etag-concurrency")).await;

    let cache_dir = tempdir().unwrap();
    let mut config = StreamingDiskConfig::new(url, cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.chunk_size = 1024;
    config.options.read_ahead_chunks = 0;
    config.options.max_concurrent_fetches = 0;

    let err = StreamingDisk::open(config)
        .await
        .err()
        .expect("expected error");
    assert!(matches!(err, StreamingDiskError::Protocol(_)));

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "current_thread")]
async fn reject_zero_max_retries() {
    let image: Vec<u8> = (0..2048).map(|i| (i % 251) as u8).collect();
    let (url, _state, shutdown) =
        start_range_server_with_options(image, RangeServerOptions::new("etag-retries")).await;

    let cache_dir = tempdir().unwrap();
    let mut config = StreamingDiskConfig::new(url, cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.chunk_size = 1024;
    config.options.read_ahead_chunks = 0;
    config.options.max_retries = 0;

    let err = StreamingDisk::open(config)
        .await
        .err()
        .expect("expected error");
    assert!(matches!(err, StreamingDiskError::Protocol(_)));

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "current_thread")]
async fn reject_excessive_max_retries() {
    let image: Vec<u8> = (0..2048).map(|i| (i % 251) as u8).collect();
    let (url, _state, shutdown) =
        start_range_server_with_options(image, RangeServerOptions::new("etag-max-retries")).await;

    let cache_dir = tempdir().unwrap();
    let mut config = StreamingDiskConfig::new(url, cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.chunk_size = 1024;
    config.options.read_ahead_chunks = 0;
    config.options.max_retries = 33;

    let err = StreamingDisk::open(config)
        .await
        .err()
        .expect("expected error");
    assert!(matches!(err, StreamingDiskError::Protocol(_)));

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "current_thread")]
async fn reject_excessive_max_concurrent_fetches_count() {
    let image: Vec<u8> = (0..2048).map(|i| (i % 251) as u8).collect();
    let (url, _state, shutdown) =
        start_range_server_with_options(image, RangeServerOptions::new("etag-max-concurrency"))
            .await;

    let cache_dir = tempdir().unwrap();
    let mut config = StreamingDiskConfig::new(url, cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.chunk_size = 1024;
    config.options.read_ahead_chunks = 0;
    config.options.max_retries = 1;
    config.options.max_concurrent_fetches = 129;

    let err = StreamingDisk::open(config)
        .await
        .err()
        .expect("expected error");
    assert!(matches!(err, StreamingDiskError::Protocol(_)));

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "current_thread")]
async fn reject_excessive_max_concurrent_fetches_inflight_bytes() {
    // Use a moderate image size so `min(chunk_size, total_size) == chunk_size` while keeping test
    // memory usage reasonable.
    let image = vec![0u8; 8 * 1024 * 1024]; // 8 MiB
    let (url, _state, shutdown) =
        start_range_server_with_options(image, RangeServerOptions::new("etag-inflight-bytes"))
            .await;

    let cache_dir = tempdir().unwrap();
    let mut config = StreamingDiskConfig::new(url, cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.chunk_size = 8 * 1024 * 1024; // 8 MiB
    config.options.read_ahead_chunks = 0;
    config.options.max_retries = 1;
    // 65 * 8 MiB = 520 MiB > 512 MiB cap
    config.options.max_concurrent_fetches = 65;

    let err = StreamingDisk::open(config)
        .await
        .err()
        .expect("expected error");
    assert!(matches!(err, StreamingDiskError::Protocol(_)));

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "current_thread")]
async fn reject_excessive_read_ahead_chunks_count() {
    let image: Vec<u8> = (0..2048).map(|i| (i % 251) as u8).collect();
    let (url, _state, shutdown) =
        start_range_server_with_options(image, RangeServerOptions::new("etag-read-ahead-count"))
            .await;

    let cache_dir = tempdir().unwrap();
    let mut config = StreamingDiskConfig::new(url, cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.chunk_size = 1024;
    config.options.read_ahead_chunks = 1025;

    let err = StreamingDisk::open(config)
        .await
        .err()
        .expect("expected error");
    assert!(matches!(err, StreamingDiskError::Protocol(_)));

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "current_thread")]
async fn reject_excessive_read_ahead_chunks_bytes() {
    let image: Vec<u8> = (0..2048).map(|i| (i % 251) as u8).collect();
    let (url, _state, shutdown) =
        start_range_server_with_options(image, RangeServerOptions::new("etag-read-ahead-bytes"))
            .await;

    let cache_dir = tempdir().unwrap();
    let mut config = StreamingDiskConfig::new(url, cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.chunk_size = 1024 * 1024; // 1 MiB
    config.options.read_ahead_chunks = 513; // 513 MiB > 512 MiB cap

    let err = StreamingDisk::open(config)
        .await
        .err()
        .expect("expected error");
    assert!(matches!(err, StreamingDiskError::Protocol(_)));

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "current_thread")]
async fn missing_required_header_fails_open_with_http_error() {
    let image: Vec<u8> = (0..2048).map(|i| (i % 251) as u8).collect();
    let (url, _state, shutdown) = start_range_server_with_options(
        image,
        RangeServerOptions {
            required_header: Some(("x-test-auth", "secret")),
            ..RangeServerOptions::new("etag-auth-required")
        },
    )
    .await;

    let cache_dir = tempdir().unwrap();
    let mut config = StreamingDiskConfig::new(url, cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.chunk_size = 1024;
    config.options.read_ahead_chunks = 0;
    config.options.max_retries = 1;

    let err = StreamingDisk::open(config).await.err().unwrap();
    assert!(matches!(err, StreamingDiskError::HttpStatus { .. }));

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "current_thread")]
async fn open_fails_when_config_validator_mismatches_remote_etag() {
    let image: Vec<u8> = (0..2048).map(|i| (i % 251) as u8).collect();
    let (url, _state, shutdown) =
        start_range_server_with_options(image, RangeServerOptions::new("etag-actual")).await;

    let cache_dir = tempdir().unwrap();
    let mut config = StreamingDiskConfig::new(url, cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.chunk_size = 1024;
    config.options.read_ahead_chunks = 0;
    config.options.max_retries = 1;
    config.validator = Some("etag-expected".to_string());

    let err = StreamingDisk::open(config).await.err().unwrap();
    assert!(matches!(err, StreamingDiskError::ValidatorMismatch { .. }));

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
        "http://127.0.0.1:{port}/image.raw?token=supersecret"
    ))
    .unwrap();
    let cache_dir = tempdir().unwrap();
    let config = StreamingDiskConfig::new(url, cache_dir.path());
    let err = StreamingDisk::open(config).await.err().unwrap();
    let StreamingDiskError::Http(msg) = err else {
        panic!("expected Http error, got {err:?}");
    };
    assert!(
        !msg.contains("token=supersecret"),
        "error message should not contain query tokens: {msg}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn last_modified_is_used_as_validator_when_etag_missing() {
    let image: Vec<u8> = (0..(4096 + 123)).map(|i| (i % 251) as u8).collect();
    let last_modified = "Mon, 01 Jan 2024 00:00:00 GMT";
    let (url, state, shutdown) = start_last_modified_server(image.clone(), last_modified).await;

    let cache_dir = tempdir().unwrap();
    let mut config = StreamingDiskConfig::new(url.clone(), cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.chunk_size = 1024;
    config.options.read_ahead_chunks = 0;

    let disk = StreamingDisk::open(config.clone()).await.unwrap();
    assert_eq!(disk.validator(), Some(last_modified));

    let mut buf = vec![0u8; 200];
    disk.read_at(1000, &mut buf).await.unwrap();
    assert_eq!(&buf[..], &image[1000..1200]);
    assert_eq!(state.counters.get_range.load(Ordering::SeqCst), 2);
    drop(disk);

    let mut url2 = url.clone();
    url2.set_query(Some("token=ignored"));
    let mut config2 = config;
    config2.url = url2;
    let disk2 = StreamingDisk::open(config2).await.unwrap();
    assert_eq!(disk2.validator(), Some(last_modified));
    let mut buf2 = vec![0u8; 200];
    disk2.read_at(1000, &mut buf2).await.unwrap();
    assert_eq!(&buf2[..], &image[1000..1200]);
    assert_eq!(
        state.counters.get_range.load(Ordering::SeqCst),
        2,
        "cache should persist across runs when validator comes from Last-Modified"
    );

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "current_thread")]
async fn range_not_supported_is_reported_when_server_ignores_range() {
    let image: Vec<u8> = (0..2048).map(|i| (i % 251) as u8).collect();
    let (url, _state, shutdown) = start_range_server_with_options(
        image.clone(),
        RangeServerOptions {
            ignore_range: true,
            ..RangeServerOptions::new("etag-norange")
        },
    )
    .await;

    let cache_dir = tempdir().unwrap();
    let mut config = StreamingDiskConfig::new(url, cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.chunk_size = 1024;
    config.options.read_ahead_chunks = 0;
    config.options.max_retries = 1;

    let disk = StreamingDisk::open(config).await.unwrap();
    let mut buf = vec![0u8; 16];
    let err = disk.read_at(0, &mut buf).await.err().unwrap();
    assert!(matches!(err, StreamingDiskError::RangeNotSupported));

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "current_thread")]
async fn content_range_mismatch_is_protocol_error() {
    let image: Vec<u8> = (0..2048).map(|i| (i % 251) as u8).collect();
    let (url, _state, shutdown) = start_range_server_with_options(
        image.clone(),
        RangeServerOptions {
            wrong_content_range: true,
            ..RangeServerOptions::new("etag-badcr")
        },
    )
    .await;

    let cache_dir = tempdir().unwrap();
    let mut config = StreamingDiskConfig::new(url, cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.chunk_size = 1024;
    config.options.read_ahead_chunks = 0;
    config.options.max_retries = 1;

    let disk = StreamingDisk::open(config).await.unwrap();
    let mut buf = vec![0u8; 16];
    let err = disk.read_at(0, &mut buf).await.err().unwrap();
    assert!(matches!(err, StreamingDiskError::Protocol(_)));

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "current_thread")]
async fn content_range_total_star_is_accepted() {
    let image: Vec<u8> = (0..2048).map(|i| (i % 251) as u8).collect();
    let (url, _state, shutdown) = start_range_server_with_options(
        image.clone(),
        RangeServerOptions {
            content_range_total_star: true,
            ..RangeServerOptions::new("etag-total-star")
        },
    )
    .await;

    let cache_dir = tempdir().unwrap();
    let mut config = StreamingDiskConfig::new(url, cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.chunk_size = 1024;
    config.options.read_ahead_chunks = 0;
    config.options.max_retries = 1;

    let disk = StreamingDisk::open(config).await.unwrap();
    let mut buf = vec![0u8; 32];
    disk.read_at(0, &mut buf).await.unwrap();
    assert_eq!(&buf[..], &image[0..32]);

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "current_thread")]
async fn content_encoding_is_rejected() {
    let image: Vec<u8> = (0..2048).map(|i| (i % 251) as u8).collect();
    let (url, _state, shutdown) = start_range_server_with_options(
        image.clone(),
        RangeServerOptions {
            content_encoding: Some("gzip"),
            ..RangeServerOptions::new("etag-encoding")
        },
    )
    .await;

    let cache_dir = tempdir().unwrap();
    let mut config = StreamingDiskConfig::new(url, cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.chunk_size = 1024;
    config.options.read_ahead_chunks = 0;
    config.options.max_retries = 1;

    let disk = StreamingDisk::open(config).await.unwrap();
    let mut buf = vec![0u8; 16];
    let err = disk.read_at(0, &mut buf).await.err().unwrap();
    assert!(matches!(err, StreamingDiskError::Protocol(_)));

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "current_thread")]
async fn cache_invalidates_when_cache_backend_changes() {
    let image: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
    let (url, state, shutdown) =
        start_range_server_with_options(image.clone(), RangeServerOptions::new("etag-backend"))
            .await;

    let cache_dir = tempdir().unwrap();
    let mut config = StreamingDiskConfig::new(url.clone(), cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.chunk_size = 1024;
    config.options.read_ahead_chunks = 0;
    config.options.max_retries = 1;

    let disk = StreamingDisk::open(config).await.unwrap();
    let mut buf = vec![0u8; 16];
    disk.read_at(0, &mut buf).await.unwrap();
    assert_eq!(&buf[..], &image[0..16]);
    assert_eq!(state.counters.get_range.load(Ordering::SeqCst), 1);
    drop(disk);

    // Re-open the same cache directory but with a different backend. If we were to reuse the
    // previous `downloaded` bitmap, we'd treat the chunk as cached and read zeros from the new
    // sparse file backend.
    let mut config2 = StreamingDiskConfig::new(url, cache_dir.path());
    config2.cache_backend = StreamingCacheBackend::SparseFile;
    config2.options.chunk_size = 1024;
    config2.options.read_ahead_chunks = 0;
    config2.options.max_retries = 1;

    let disk2 = StreamingDisk::open(config2).await.unwrap();
    let mut buf2 = vec![0u8; 16];
    disk2.read_at(0, &mut buf2).await.unwrap();
    assert_eq!(&buf2[..], &image[0..16]);
    assert_eq!(
        state.counters.get_range.load(Ordering::SeqCst),
        2,
        "cache backend change should invalidate downloaded ranges and re-fetch"
    );

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "current_thread")]
async fn corrupt_cache_metadata_is_treated_as_invalidation() {
    let image: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
    let (url, state, shutdown) =
        start_range_server_with_options(image.clone(), RangeServerOptions::new("etag-corrupt"))
            .await;

    let cache_dir = tempdir().unwrap();
    let mut config = StreamingDiskConfig::new(url.clone(), cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.chunk_size = 1024;
    config.options.read_ahead_chunks = 0;
    config.options.max_retries = 1;

    let disk = StreamingDisk::open(config).await.unwrap();
    let mut buf = vec![0u8; 16];
    disk.read_at(0, &mut buf).await.unwrap();
    assert_eq!(&buf[..], &image[0..16]);
    assert_eq!(state.counters.get_range.load(Ordering::SeqCst), 1);
    drop(disk);

    // Corrupt the on-disk metadata file; the next open should not fail.
    let meta_path = cache_dir.path().join("streaming-cache-meta.json");
    std::fs::write(&meta_path, "{not valid json").unwrap();

    let mut config2 = StreamingDiskConfig::new(url, cache_dir.path());
    config2.cache_backend = StreamingCacheBackend::Directory;
    config2.options.chunk_size = 1024;
    config2.options.read_ahead_chunks = 0;
    config2.options.max_retries = 1;

    let disk2 = StreamingDisk::open(config2).await.unwrap();
    let mut buf2 = vec![0u8; 16];
    disk2.read_at(0, &mut buf2).await.unwrap();
    assert_eq!(&buf2[..], &image[0..16]);
    assert_eq!(
        state.counters.get_range.load(Ordering::SeqCst),
        2,
        "corrupt metadata should be treated as cache invalidation"
    );

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "current_thread")]
async fn semantically_invalid_downloaded_ranges_in_meta_are_treated_as_invalidation() {
    let image: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
    let (url, state, shutdown) = start_range_server_with_options(
        image.clone(),
        RangeServerOptions::new("etag-invalid-downloaded"),
    )
    .await;

    let cache_dir = tempdir().unwrap();
    let mut config = StreamingDiskConfig::new(url.clone(), cache_dir.path());
    config.cache_backend = StreamingCacheBackend::SparseFile;
    config.options.chunk_size = 1024;
    config.options.read_ahead_chunks = 0;
    config.options.max_retries = 1;

    // Create the on-disk cache + metadata, but do not read any data.
    let disk = StreamingDisk::open(config).await.unwrap();
    drop(disk);

    // Corrupt the metadata in a *valid JSON* way by inserting an out-of-bounds downloaded range
    // while still claiming the first chunk is cached. If we trusted this, the sparse-file backend
    // would read zeros without performing any HTTP requests.
    let meta_path = cache_dir.path().join("streaming-cache-meta.json");
    let raw = std::fs::read_to_string(&meta_path).unwrap();
    let mut meta: serde_json::Value = serde_json::from_str(&raw).unwrap();
    meta["downloaded"]["ranges"] = serde_json::json!([
        { "start": 0u64, "end": 1024u64 },
        { "start": 4096u64, "end": 5120u64 },
    ]);
    std::fs::write(&meta_path, serde_json::to_string(&meta).unwrap()).unwrap();

    let mut config2 = StreamingDiskConfig::new(url, cache_dir.path());
    config2.cache_backend = StreamingCacheBackend::SparseFile;
    config2.options.chunk_size = 1024;
    config2.options.read_ahead_chunks = 0;
    config2.options.max_retries = 1;

    let disk2 = StreamingDisk::open(config2).await.unwrap();
    let mut buf = vec![0u8; 16];
    disk2.read_at(0, &mut buf).await.unwrap();
    assert_eq!(&buf[..], &image[0..16]);
    assert_eq!(
        state.counters.get_range.load(Ordering::SeqCst),
        1,
        "invalid downloaded ranges should invalidate cache metadata and trigger re-fetch"
    );

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "current_thread")]
async fn weak_etag_does_not_break_range_fetches() {
    // RFC 9110 disallows weak validators in `If-Range`. Some servers respond with `200 OK` (full
    // representation) instead of `206 Partial Content` when clients send `If-Range: W/"..."`.
    //
    // `StreamingDisk` should omit `If-Range` when the validator is a weak ETag to avoid
    // misclassifying the server as not supporting Range.
    let image: Vec<u8> = (0..2048).map(|i| (i % 251) as u8).collect();
    let (url, _state, shutdown) = start_range_server_with_options(
        image.clone(),
        RangeServerOptions {
            enforce_strong_if_range: true,
            ..RangeServerOptions::new(r#"W/"etag-weak""#)
        },
    )
    .await;

    let cache_dir = tempdir().unwrap();
    let mut config = StreamingDiskConfig::new(url, cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.chunk_size = 1024;
    config.options.read_ahead_chunks = 0;
    config.options.max_retries = 1;

    let disk = StreamingDisk::open(config).await.unwrap();
    let mut buf = vec![0u8; 16];
    disk.read_at(0, &mut buf).await.unwrap();
    assert_eq!(&buf[..], &image[0..16]);

    let _ = shutdown.send(());
}

#[tokio::test(flavor = "current_thread")]
async fn weak_etag_change_is_detected_even_without_if_range() {
    // When the validator is a weak ETag, `StreamingDisk` omits `If-Range` to avoid servers
    // treating it as a mismatch per RFC 9110. We still want to detect when the server starts
    // serving a different version of the representation, so we compare the validator on `206`
    // responses when present.
    let image: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
    let (url, state, shutdown) = start_range_server_with_options(
        image.clone(),
        RangeServerOptions {
            enforce_strong_if_range: true,
            ..RangeServerOptions::new(r#"W/"etag-v1""#)
        },
    )
    .await;

    let cache_dir = tempdir().unwrap();
    let mut config = StreamingDiskConfig::new(url, cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.chunk_size = 1024;
    config.options.read_ahead_chunks = 0;
    config.options.max_retries = 1;

    let disk = StreamingDisk::open(config).await.unwrap();
    let mut buf = vec![0u8; 16];
    disk.read_at(0, &mut buf).await.unwrap();
    assert_eq!(&buf[..], &image[0..16]);

    // Simulate the remote changing while the disk is open.
    *state.etag.lock().unwrap() = r#"W/"etag-v2""#.to_string();

    let err = disk.read_at(1024, &mut buf).await.err().unwrap();
    assert!(matches!(err, StreamingDiskError::ValidatorMismatch { .. }));

    let _ = shutdown.send(());
}
