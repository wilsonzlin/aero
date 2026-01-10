use std::{
    convert::Infallible,
    net::TcpListener,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use emulator::io::storage::{
    backends::streaming::{StreamingDisk, StreamingDiskOptions},
    error::StorageError,
    metadata::InMemoryMetadataStore,
    sparse::InMemoryStore,
    SECTOR_SIZE,
};
use hyper::{
    header::{HeaderValue, ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, RANGE},
    service::{make_service_fn, service_fn},
    Body, Method, Request, Response, Server, StatusCode,
};

fn make_test_data(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 251) as u8).collect()
}

async fn spawn_server(
    data: Arc<Vec<u8>>,
    supports_range: bool,
) -> (String, Arc<AtomicU64>, tokio::task::JoinHandle<()>) {
    let range_gets = Arc::new(AtomicU64::new(0));
    let range_gets_for_svc = range_gets.clone();

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
    let addr = listener.local_addr().expect("server addr");

    let make_svc = make_service_fn(move |_| {
        let data = data.clone();
        let range_gets = range_gets_for_svc.clone();
        async move {
            Ok::<_, Infallible>(service_fn(move |req: Request<Body>| {
                let data = data.clone();
                let range_gets = range_gets.clone();
                async move { Ok::<_, Infallible>(handle(req, data, range_gets, supports_range)) }
            }))
        }
    });

    let server = Server::from_tcp(listener)
        .expect("server from_tcp")
        .serve(make_svc);

    let handle = tokio::spawn(async move {
        let _ = server.await;
    });

    (format!("http://{addr}/image.bin"), range_gets, handle)
}

fn handle(
    req: Request<Body>,
    data: Arc<Vec<u8>>,
    range_gets: Arc<AtomicU64>,
    supports_range: bool,
) -> Response<Body> {
    let total = data.len() as u64;

    let mut base = Response::builder();
    if supports_range {
        base = base.header(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    }

    match *req.method() {
        Method::HEAD => {
            return base
                .status(StatusCode::OK)
                .header(CONTENT_LENGTH, total.to_string())
                .body(Body::empty())
                .expect("head response");
        }
        Method::GET => {}
        _ => {
            return base
                .status(StatusCode::METHOD_NOT_ALLOWED)
                .body(Body::empty())
                .expect("405 response");
        }
    }

    let range = req
        .headers()
        .get(RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|_| supports_range);

    if let Some(range) = range {
        // Format: bytes=start-end (end optional).
        if !range.starts_with("bytes=") {
            return base
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(CONTENT_RANGE, format!("bytes */{total}"))
                .body(Body::empty())
                .expect("416 response");
        }

        let spec = &range["bytes=".len()..];
        let (start_s, end_s) = spec.split_once('-').unwrap_or((spec, ""));
        let start: u64 = match start_s.parse() {
            Ok(v) => v,
            Err(_) => {
                return base
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(CONTENT_RANGE, format!("bytes */{total}"))
                    .body(Body::empty())
                    .expect("416 response");
            }
        };
        let mut end: u64 = if end_s.is_empty() {
            total.saturating_sub(1)
        } else {
            match end_s.parse() {
                Ok(v) => v,
                Err(_) => {
                    return base
                        .status(StatusCode::RANGE_NOT_SATISFIABLE)
                        .header(CONTENT_RANGE, format!("bytes */{total}"))
                        .body(Body::empty())
                        .expect("416 response");
                }
            }
        };

        if start >= total {
            return base
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(CONTENT_RANGE, format!("bytes */{total}"))
                .body(Body::empty())
                .expect("416 response");
        }

        if end >= total {
            end = total - 1;
        }
        if end < start {
            return base
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(CONTENT_RANGE, format!("bytes */{total}"))
                .body(Body::empty())
                .expect("416 response");
        }

        range_gets.fetch_add(1, Ordering::Relaxed);
        let start_usize = start as usize;
        let end_usize = (end as usize) + 1;
        let body = data[start_usize..end_usize].to_vec();

        return base
            .status(StatusCode::PARTIAL_CONTENT)
            .header(CONTENT_RANGE, format!("bytes {start}-{end}/{total}"))
            .header(CONTENT_LENGTH, body.len().to_string())
            .body(Body::from(body))
            .expect("206 response");
    }

    // No range (or range not supported): serve full content.
    base.status(StatusCode::OK)
        .header(CONTENT_LENGTH, total.to_string())
        .body(Body::from(data.as_ref().clone()))
        .expect("200 response")
}

#[tokio::test]
async fn first_read_fetches_then_hits_cache() {
    let chunk_size = 1024u64;
    let image = Arc::new(make_test_data(4096));
    let (url, range_gets, _server) = spawn_server(image.clone(), true).await;

    let cache = Arc::new(InMemoryStore::new(image.len() as u64));
    let overlay = Arc::new(InMemoryStore::new(image.len() as u64));
    let meta = Arc::new(InMemoryMetadataStore::new());

    let disk = StreamingDisk::new(
        url,
        cache,
        overlay,
        meta,
        StreamingDiskOptions {
            chunk_size,
            read_ahead_chunks: 0,
            max_concurrent_fetches: 2,
            max_retries: 2,
            manifest: None,
        },
    )
    .await
    .unwrap();

    let mut buf = vec![0u8; 2 * SECTOR_SIZE];
    disk.read_sectors(0, &mut buf).await.unwrap();
    assert_eq!(&buf, &image[0..buf.len()]);
    assert_eq!(range_gets.load(Ordering::Relaxed), 1);

    buf.fill(0);
    disk.read_sectors(0, &mut buf).await.unwrap();
    assert_eq!(&buf, &image[0..buf.len()]);
    assert_eq!(range_gets.load(Ordering::Relaxed), 1);

    let tel = disk.telemetry_snapshot();
    assert_eq!(tel.bytes_downloaded, chunk_size);
    assert_eq!(tel.range_requests, 1);
    assert!(tel.cache_hit_chunks >= 1);
    assert!(tel.cache_miss_chunks >= 1);
}

#[tokio::test]
async fn overlay_precedence_avoids_remote_fetch() {
    let image = Arc::new(make_test_data(2048));
    let (url, range_gets, _server) = spawn_server(image.clone(), true).await;

    let cache = Arc::new(InMemoryStore::new(image.len() as u64));
    let overlay = Arc::new(InMemoryStore::new(image.len() as u64));
    let meta = Arc::new(InMemoryMetadataStore::new());

    let disk = StreamingDisk::new(
        url,
        cache,
        overlay,
        meta,
        StreamingDiskOptions {
            chunk_size: 1024,
            read_ahead_chunks: 0,
            max_concurrent_fetches: 1,
            max_retries: 2,
            manifest: None,
        },
    )
    .await
    .unwrap();

    let patch = vec![0xCC; SECTOR_SIZE];
    disk.write_sectors(0, &patch).await.unwrap();

    let mut buf = vec![0u8; SECTOR_SIZE];
    disk.read_sectors(0, &mut buf).await.unwrap();
    assert_eq!(buf, patch);

    // Entire read is covered by dirty overlay => no range GETs.
    assert_eq!(range_gets.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn rejects_server_without_range_support() {
    let image = Arc::new(make_test_data(1024));
    let (url, _range_gets, _server) = spawn_server(image.clone(), false).await;

    let cache = Arc::new(InMemoryStore::new(image.len() as u64));
    let overlay = Arc::new(InMemoryStore::new(image.len() as u64));
    let meta = Arc::new(InMemoryMetadataStore::new());

    let err = StreamingDisk::new(url, cache, overlay, meta, StreamingDiskOptions::default())
        .await
        .err()
        .expect("expected range support error");

    assert!(matches!(err, StorageError::RangeNotSupported));
}

#[tokio::test]
async fn persisted_ranges_prevent_redownload() {
    let image = Arc::new(make_test_data(4096));
    let (url, range_gets, _server) = spawn_server(image.clone(), true).await;

    let cache = Arc::new(InMemoryStore::new(image.len() as u64));
    let overlay = Arc::new(InMemoryStore::new(image.len() as u64));
    let meta = Arc::new(InMemoryMetadataStore::new());

    let disk1 = StreamingDisk::new(
        url.clone(),
        cache.clone(),
        overlay.clone(),
        meta.clone(),
        StreamingDiskOptions {
            chunk_size: 1024,
            read_ahead_chunks: 0,
            max_concurrent_fetches: 1,
            max_retries: 2,
            manifest: None,
        },
    )
    .await
    .unwrap();

    let mut buf = vec![0u8; 2 * SECTOR_SIZE];
    disk1.read_sectors(0, &mut buf).await.unwrap();
    assert_eq!(range_gets.load(Ordering::Relaxed), 1);

    range_gets.store(0, Ordering::Relaxed);

    let disk2 = StreamingDisk::new(
        url,
        cache,
        overlay,
        meta,
        StreamingDiskOptions {
            chunk_size: 1024,
            read_ahead_chunks: 0,
            max_concurrent_fetches: 1,
            max_retries: 2,
            manifest: None,
        },
    )
    .await
    .unwrap();

    buf.fill(0);
    disk2.read_sectors(0, &mut buf).await.unwrap();
    assert_eq!(&buf, &image[0..buf.len()]);

    // Disk2 should treat chunk 0 as already downloaded and not hit the network.
    assert_eq!(range_gets.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn sequential_reads_trigger_prefetch() {
    let image = Arc::new(make_test_data(4096));
    let (url, range_gets, _server) = spawn_server(image.clone(), true).await;

    let cache = Arc::new(InMemoryStore::new(image.len() as u64));
    let overlay = Arc::new(InMemoryStore::new(image.len() as u64));
    let meta = Arc::new(InMemoryMetadataStore::new());

    let disk = StreamingDisk::new(
        url,
        cache,
        overlay,
        meta,
        StreamingDiskOptions {
            chunk_size: 1024,
            read_ahead_chunks: 1,
            max_concurrent_fetches: 2,
            max_retries: 2,
            manifest: None,
        },
    )
    .await
    .unwrap();

    let mut buf = vec![0u8; 2 * SECTOR_SIZE];
    disk.read_sectors(0, &mut buf).await.unwrap();

    // First read downloads chunk 0. Prefetch should download chunk 1.
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if range_gets.load(Ordering::Relaxed) >= 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("prefetch did not complete in time");
}
