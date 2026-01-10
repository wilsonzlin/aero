#![cfg(not(target_arch = "wasm32"))]

use aero_storage::{PrefetchConfig, StreamingDisk, StreamingDiskConfig};
use hyper::header::{ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, RANGE};
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Method, Request, Response, Server, StatusCode};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
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

async fn start_range_server(
    image: Vec<u8>,
) -> (Url, Arc<Counters>, oneshot::Sender<()>) {
    let image = Arc::new(image);
    let counters = Arc::new(Counters::default());

    let make_svc = {
        let image = image.clone();
        let counters = counters.clone();
        make_service_fn(move |_conn| {
            let image = image.clone();
            let counters = counters.clone();
            async move {
                Ok::<_, Infallible>(service_fn(move |req| {
                    handle_request(req, image.clone(), counters.clone())
                }))
            }
        })
    };

    let addr: SocketAddr = ([127, 0, 0, 1], 0).into();
    let builder = Server::try_bind(&addr).expect("bind");
    let local_addr = builder.local_addr();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let server = builder
        .serve(make_svc)
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
        });

    tokio::spawn(server);

    let url = Url::parse(&format!("http://{local_addr}/image.raw")).expect("url");
    (url, counters, shutdown_tx)
}

async fn handle_request(
    req: Request<Body>,
    image: Arc<Vec<u8>>,
    counters: Arc<Counters>,
) -> Result<Response<Body>, Infallible> {
    match *req.method() {
        Method::HEAD => {
            counters.head.fetch_add(1, Ordering::SeqCst);
            let mut resp = Response::new(Body::empty());
            *resp.status_mut() = StatusCode::OK;
            resp.headers_mut()
                .insert(CONTENT_LENGTH, (image.len() as u64).to_string().parse().unwrap());
            resp.headers_mut()
                .insert(ACCEPT_RANGES, "bytes".parse().unwrap());
            return Ok(resp);
        }
        Method::GET => {
            if let Some(range_header) = req.headers().get(RANGE).and_then(|v| v.to_str().ok()) {
                counters.get_range.fetch_add(1, Ordering::SeqCst);
                match parse_range_header(range_header, image.len() as u64) {
                    Ok((start, end_exclusive)) => {
                        let end_inclusive = end_exclusive - 1;
                        let body = image[start as usize..end_exclusive as usize].to_vec();
                        let mut resp = Response::new(Body::from(body));
                        *resp.status_mut() = StatusCode::PARTIAL_CONTENT;
                        resp.headers_mut().insert(
                            CONTENT_LENGTH,
                            (end_exclusive - start).to_string().parse().unwrap(),
                        );
                        resp.headers_mut().insert(ACCEPT_RANGES, "bytes".parse().unwrap());
                        resp.headers_mut().insert(
                            CONTENT_RANGE,
                            format!("bytes {start}-{end_inclusive}/{}", image.len())
                                .parse()
                                .unwrap(),
                        );
                        return Ok(resp);
                    }
                    Err(status) => {
                        let mut resp = Response::new(Body::empty());
                        *resp.status_mut() = status;
                        return Ok(resp);
                    }
                }
            }

            counters.get_full.fetch_add(1, Ordering::SeqCst);
            let mut resp = Response::new(Body::from(image.as_ref().clone()));
            *resp.status_mut() = StatusCode::OK;
            resp.headers_mut()
                .insert(CONTENT_LENGTH, (image.len() as u64).to_string().parse().unwrap());
            resp.headers_mut()
                .insert(ACCEPT_RANGES, "bytes".parse().unwrap());
            return Ok(resp);
        }
        _ => {}
    }

    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = StatusCode::METHOD_NOT_ALLOWED;
    Ok(resp)
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

#[tokio::test]
async fn streaming_reads_and_reuses_cache() {
    let image: Vec<u8> = (0..(4096 + 123))
        .map(|i| (i % 251) as u8)
        .collect();
    let (url, counters, shutdown) = start_range_server(image.clone()).await;

    let cache_dir = tempdir().unwrap();
    let mut config = StreamingDiskConfig::new(url.clone(), cache_dir.path());
    config.block_size = 1024;
    config.cache_limit_bytes = None;
    config.prefetch = PrefetchConfig {
        enabled: false,
        sequential_distance_blocks: 0,
    };

    let disk = StreamingDisk::open(config.clone()).await.unwrap();
    assert_eq!(disk.total_size() as usize, image.len());

    let mut buf = vec![0u8; 200];
    disk.read_at(1000, &mut buf).await.unwrap();
    assert_eq!(&buf[..], &image[1000..1200]);

    // Offset 1000..1200 touches blocks 0 and 1, so we expect 2 range GETs.
    assert_eq!(counters.get_range.load(Ordering::SeqCst), 2);

    let mut buf2 = vec![0u8; 200];
    disk.read_at(1000, &mut buf2).await.unwrap();
    assert_eq!(&buf2[..], &image[1000..1200]);
    assert_eq!(
        counters.get_range.load(Ordering::SeqCst),
        2,
        "second read should be served from cache"
    );

    drop(disk);

    // Re-open with the same cache directory; should still avoid extra range GETs.
    let disk2 = StreamingDisk::open(config).await.unwrap();
    let mut buf3 = vec![0u8; 200];
    disk2.read_at(1000, &mut buf3).await.unwrap();
    assert_eq!(&buf3[..], &image[1000..1200]);
    assert_eq!(
        counters.get_range.load(Ordering::SeqCst),
        2,
        "cache should persist across runs"
    );

    let _ = shutdown.send(());
}

#[tokio::test]
async fn cache_limit_eviction_refetches() {
    let image: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
    let (url, counters, shutdown) = start_range_server(image.clone()).await;

    let cache_dir = tempdir().unwrap();
    let mut config = StreamingDiskConfig::new(url, cache_dir.path());
    config.block_size = 1024;
    config.cache_limit_bytes = Some(1024); // exactly one block
    config.prefetch = PrefetchConfig {
        enabled: false,
        sequential_distance_blocks: 0,
    };

    let disk = StreamingDisk::open(config).await.unwrap();

    let mut buf = vec![0u8; 16];
    disk.read_at(0, &mut buf).await.unwrap();
    assert_eq!(&buf[..], &image[0..16]);

    disk.read_at(1024, &mut buf).await.unwrap();
    assert_eq!(&buf[..], &image[1024..1040]);

    // Two different blocks were read, so at least 2 range GETs.
    assert_eq!(counters.get_range.load(Ordering::SeqCst), 2);

    // Cache limit is 1 block, so block 0 should have been evicted by now.
    disk.read_at(0, &mut buf).await.unwrap();
    assert_eq!(&buf[..], &image[0..16]);
    assert_eq!(
        counters.get_range.load(Ordering::SeqCst),
        3,
        "evicted block should be re-fetched"
    );

    let _ = shutdown.send(());
}
