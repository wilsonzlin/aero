#![cfg(not(target_arch = "wasm32"))]

use std::{
    convert::Infallible,
    net::TcpListener,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use emulator::io::storage::{
    backends::streaming::{StreamingDisk, StreamingDiskOptions},
    metadata::InMemoryMetadataStore,
    sparse::FileStore,
    StorageError,
};
use hyper::{
    header::{HeaderValue, ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, RANGE},
    service::{make_service_fn, service_fn},
    Body, Method, Request, Response, Server, StatusCode,
};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};

const FILE_SIZE: u64 = 5_368_709_120; // 5 GiB
const HIGH_OFFSET: u64 = 4_294_967_296 + 123; // 2^32 + 123

const SENTINEL_HIGH: &[u8] = b"AERO_RANGE_4GB";

#[derive(Default)]
struct Counters {
    head: AtomicUsize,
    range_get: AtomicUsize,
    last_range_header: Mutex<Option<String>>,
}

async fn spawn_sparse_file_server(
    file_path: Arc<PathBuf>,
    total_size: u64,
    counters: Arc<Counters>,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
    let addr = listener.local_addr().expect("server addr");

    let make_svc = make_service_fn(move |_| {
        let file_path = file_path.clone();
        let counters = counters.clone();
        async move {
            Ok::<_, Infallible>(service_fn(move |req: Request<Body>| {
                let file_path = file_path.clone();
                let counters = counters.clone();
                async move {
                    Ok::<_, Infallible>(handle(req, file_path, total_size, counters).await)
                }
            }))
        }
    });

    let server = Server::from_tcp(listener)
        .expect("server from_tcp")
        .serve(make_svc);

    let handle = tokio::spawn(async move {
        let _ = server.await;
    });

    (format!("http://{addr}/image.bin"), handle)
}

async fn handle(
    req: Request<Body>,
    file_path: Arc<PathBuf>,
    total_size: u64,
    counters: Arc<Counters>,
) -> Response<Body> {
    match *req.method() {
        Method::HEAD => {
            counters.head.fetch_add(1, Ordering::SeqCst);
            return Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_LENGTH, total_size.to_string())
                .header(ACCEPT_RANGES, HeaderValue::from_static("bytes"))
                .body(Body::empty())
                .expect("head response");
        }
        Method::GET => {}
        _ => {
            return Response::builder()
                .status(StatusCode::METHOD_NOT_ALLOWED)
                .body(Body::empty())
                .expect("405 response");
        }
    }

    let range = req.headers().get(RANGE).and_then(|v| v.to_str().ok());
    let Some(range) = range else {
        return Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(Body::empty())
            .expect("400 response");
    };

    {
        let mut slot = counters.last_range_header.lock().unwrap();
        *slot = Some(range.trim().to_string());
    }
    counters.range_get.fetch_add(1, Ordering::SeqCst);

    let (start, end_exclusive) = match parse_single_range(range, total_size) {
        Ok(v) => v,
        Err(status) => {
            return Response::builder()
                .status(status)
                .body(Body::empty())
                .unwrap_or_else(|_| {
                    let mut resp = Response::new(Body::empty());
                    *resp.status_mut() = status;
                    resp
                });
        }
    };

    let len = (end_exclusive - start) as usize;

    let mut file = tokio::fs::File::open(file_path.as_ref())
        .await
        .expect("open image file");
    file.seek(SeekFrom::Start(start)).await.expect("seek");
    let mut buf = vec![0u8; len];
    file.read_exact(&mut buf).await.expect("read range");

    let end_inclusive = end_exclusive - 1;

    Response::builder()
        .status(StatusCode::PARTIAL_CONTENT)
        .header(CONTENT_RANGE, format!("bytes {start}-{end_inclusive}/{total_size}"))
        .header(CONTENT_LENGTH, len.to_string())
        .header(ACCEPT_RANGES, HeaderValue::from_static("bytes"))
        .body(Body::from(buf))
        .expect("206 response")
}

fn parse_single_range(header_value: &str, size: u64) -> Result<(u64, u64), StatusCode> {
    let header_value = header_value.trim();
    let Some(spec) = header_value.strip_prefix("bytes=") else {
        return Err(StatusCode::BAD_REQUEST);
    };
    if spec.contains(',') {
        return Err(StatusCode::BAD_REQUEST);
    }
    let (start_s, end_s) = spec.split_once('-').ok_or(StatusCode::BAD_REQUEST)?;
    if start_s.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    if size == 0 {
        return Err(StatusCode::RANGE_NOT_SATISFIABLE);
    }

    let start: u64 = start_s.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    if start >= size {
        return Err(StatusCode::RANGE_NOT_SATISFIABLE);
    }

    let last = size - 1;
    let mut end: u64 = if end_s.is_empty() {
        last
    } else {
        end_s.parse().map_err(|_| StatusCode::BAD_REQUEST)?
    };
    if end < start {
        return Err(StatusCode::BAD_REQUEST);
    }
    if end > last {
        end = last;
    }

    Ok((start, end + 1))
}

#[tokio::test]
async fn streaming_disk_requests_ranges_beyond_4gib_without_truncation() -> Result<(), StorageError> {
    let tmp = tempfile::tempdir().unwrap();
    let image_path = tmp.path().join("image.bin");

    // Create sparse 5GiB image with a sentinel beyond 2^32.
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&image_path)
        .await
        .unwrap();
    file.set_len(FILE_SIZE).await.unwrap();
    file.seek(SeekFrom::Start(HIGH_OFFSET)).await.unwrap();
    file.write_all(SENTINEL_HIGH).await.unwrap();
    file.flush().await.unwrap();
    drop(file);

    let counters = Arc::new(Counters::default());
    let (url, server_handle) =
        spawn_sparse_file_server(Arc::new(image_path), FILE_SIZE, counters.clone()).await;

    // File-backed cache/overlay so we can use a >4GiB virtual disk without allocating RAM.
    let cache_path = tmp.path().join("cache.bin");
    let overlay_path = tmp.path().join("overlay.bin");
    let cache = Arc::new(FileStore::create(&cache_path, FILE_SIZE)?);
    let overlay = Arc::new(FileStore::create(&overlay_path, FILE_SIZE)?);
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
            max_retries: 1,
            manifest: None,
        },
    )
    .await?;

    let mut buf = vec![0u8; SENTINEL_HIGH.len()];
    disk.read_at(HIGH_OFFSET, &mut buf).await?;
    assert_eq!(&buf[..], SENTINEL_HIGH);

    // Ensure the Range header string uses full 64-bit decimal offsets.
    let expected_start = (HIGH_OFFSET / 1024) * 1024;
    let expected_end = expected_start + 1024 - 1;
    let seen = counters
        .last_range_header
        .lock()
        .unwrap()
        .clone()
        .expect("expected at least one range request");
    assert_eq!(seen, format!("bytes={expected_start}-{expected_end}"));

    assert!(counters.head.load(Ordering::SeqCst) >= 1);
    assert!(counters.range_get.load(Ordering::SeqCst) >= 1);

    server_handle.abort();
    Ok(())
}
