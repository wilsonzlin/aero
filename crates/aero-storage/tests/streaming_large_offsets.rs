#![cfg(not(target_arch = "wasm32"))]

use aero_storage::{StreamingCacheBackend, StreamingDisk, StreamingDiskConfig};
use hyper::header::{ACCEPT_RANGES, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_RANGE, ETAG, RANGE};
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Method, Request, Response, Server, StatusCode};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tempfile::tempdir;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};
use tokio::sync::oneshot;
use url::Url;

const FOUR_GIB: u64 = 4_294_967_296; // 2^32
const FILE_SIZE: u64 = FOUR_GIB + 1024; // just over 4GiB (avoid a 5GiB sparse file in tests)
const HIGH_OFFSET: u64 = FOUR_GIB + 123; // 2^32 + 123

const SENTINEL_HIGH: &[u8] = b"AERO_RANGE_4GB";
const SENTINEL_END: &[u8] = b"AERO_RANGE_END";

struct Counters {
    head: AtomicUsize,
    range_get: AtomicUsize,
    last_range_header: Mutex<Option<String>>,
}

impl Default for Counters {
    fn default() -> Self {
        Self {
            head: AtomicUsize::new(0),
            range_get: AtomicUsize::new(0),
            last_range_header: Mutex::new(None),
        }
    }
}

async fn start_sparse_range_server(
    file_path: Arc<std::path::PathBuf>,
    total_size: u64,
) -> (Url, Arc<Counters>, oneshot::Sender<()>) {
    let counters = Arc::new(Counters::default());

    let make_svc = {
        let counters = counters.clone();
        make_service_fn(move |_conn| {
            let counters = counters.clone();
            let file_path = file_path.clone();
            async move {
                Ok::<_, Infallible>(service_fn(move |req| {
                    handle_request(req, file_path.clone(), total_size, counters.clone())
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

    let url = Url::parse(&format!("http://{local_addr}/image.img")).expect("url");
    (url, counters, shutdown_tx)
}

async fn handle_request(
    req: Request<Body>,
    file_path: Arc<std::path::PathBuf>,
    total_size: u64,
    counters: Arc<Counters>,
) -> Result<Response<Body>, Infallible> {
    match *req.method() {
        Method::HEAD => {
            counters.head.fetch_add(1, Ordering::SeqCst);
            let mut resp = Response::new(Body::empty());
            *resp.status_mut() = StatusCode::OK;
            resp.headers_mut()
                .insert(CONTENT_LENGTH, total_size.to_string().parse().unwrap());
            resp.headers_mut()
                .insert(ACCEPT_RANGES, "bytes".parse().unwrap());
            resp.headers_mut()
                .insert(ETAG, "\"large-offset\"".parse().unwrap());
            resp.headers_mut()
                .insert(CACHE_CONTROL, "no-transform".parse().unwrap());
            return Ok(resp);
        }
        Method::GET => {}
        _ => {
            let mut resp = Response::new(Body::empty());
            *resp.status_mut() = StatusCode::METHOD_NOT_ALLOWED;
            return Ok(resp);
        }
    }

    let Some(range_header) = req.headers().get(RANGE).and_then(|v| v.to_str().ok()) else {
        let mut resp = Response::new(Body::empty());
        *resp.status_mut() = StatusCode::BAD_REQUEST;
        return Ok(resp);
    };

    let range_header = range_header.trim().to_string();
    {
        let mut slot = counters.last_range_header.lock().unwrap();
        *slot = Some(range_header.clone());
    }
    counters.range_get.fetch_add(1, Ordering::SeqCst);

    let (start, end_exclusive) = match parse_range_header(&range_header, total_size) {
        Ok(v) => v,
        Err(status) => {
            let mut resp = Response::new(Body::empty());
            *resp.status_mut() = status;
            return Ok(resp);
        }
    };

    let len = (end_exclusive - start) as usize;
    let mut file = tokio::fs::File::open(file_path.as_ref()).await.unwrap();
    file.seek(SeekFrom::Start(start)).await.unwrap();
    let mut buf = vec![0u8; len];
    file.read_exact(&mut buf).await.unwrap();

    let end_inclusive = end_exclusive - 1;
    let mut resp = Response::new(Body::from(buf));
    *resp.status_mut() = StatusCode::PARTIAL_CONTENT;
    resp.headers_mut()
        .insert(CACHE_CONTROL, "no-transform".parse().unwrap());
    resp.headers_mut().insert(
        CONTENT_LENGTH,
        (end_exclusive - start).to_string().parse().unwrap(),
    );
    resp.headers_mut()
        .insert(ACCEPT_RANGES, "bytes".parse().unwrap());
    resp.headers_mut()
        .insert(ETAG, "\"large-offset\"".parse().unwrap());
    resp.headers_mut()
        .insert(CACHE_CONTROL, "no-transform".parse().unwrap());
    resp.headers_mut().insert(
        CONTENT_RANGE,
        format!("bytes {start}-{end_inclusive}/{total_size}")
            .parse()
            .unwrap(),
    );
    Ok(resp)
}

fn parse_range_header(header: &str, total_size: u64) -> Result<(u64, u64), StatusCode> {
    let header = header.trim();
    let Some(spec) = header.strip_prefix("bytes=") else {
        return Err(StatusCode::BAD_REQUEST);
    };

    let (start_s, end_s) = spec.split_once('-').ok_or(StatusCode::BAD_REQUEST)?;
    if start_s.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let start: u64 = start_s.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let mut end_inclusive: u64 = if end_s.is_empty() {
        total_size.saturating_sub(1)
    } else {
        end_s.parse().map_err(|_| StatusCode::BAD_REQUEST)?
    };

    if total_size == 0 || start >= total_size {
        return Err(StatusCode::RANGE_NOT_SATISFIABLE);
    }
    let last = total_size - 1;
    if end_inclusive > last {
        end_inclusive = last;
    }
    if end_inclusive < start {
        return Err(StatusCode::RANGE_NOT_SATISFIABLE);
    }
    Ok((start, end_inclusive + 1))
}

#[tokio::test(flavor = "current_thread")]
async fn streaming_disk_reads_offsets_beyond_4gib_without_truncation() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("sparse.img");

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&path)
        .await
        .unwrap();

    file.seek(SeekFrom::Start(HIGH_OFFSET)).await.unwrap();
    file.write_all(SENTINEL_HIGH).await.unwrap();

    let end_offset = FILE_SIZE - (SENTINEL_END.len() as u64);
    file.seek(SeekFrom::Start(end_offset)).await.unwrap();
    file.write_all(SENTINEL_END).await.unwrap();
    file.flush().await.unwrap();
    drop(file);

    let (url, counters, shutdown) = start_sparse_range_server(Arc::new(path), FILE_SIZE).await;

    let cache_dir = tempdir().unwrap();
    let mut config = StreamingDiskConfig::new(url, cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.chunk_size = 1024;
    config.options.read_ahead_chunks = 0;
    config.options.max_retries = 1;

    let disk = StreamingDisk::open(config).await.unwrap();

    let mut buf = vec![0u8; SENTINEL_HIGH.len()];
    disk.read_at(HIGH_OFFSET, &mut buf).await.unwrap();
    assert_eq!(&buf[..], SENTINEL_HIGH);

    let expected_start = (HIGH_OFFSET / 1024) * 1024;
    let expected_end = expected_start + 1024 - 1;
    let seen = counters
        .last_range_header
        .lock()
        .unwrap()
        .clone()
        .expect("expected at least one range GET");
    assert_eq!(
        seen,
        format!("bytes={expected_start}-{expected_end}"),
        "Range header should use full 64-bit decimal offsets"
    );

    assert!(counters.head.load(Ordering::SeqCst) >= 1);
    assert!(counters.range_get.load(Ordering::SeqCst) >= 1);

    let _ = shutdown.send(());
}
