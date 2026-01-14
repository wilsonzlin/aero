#![cfg(not(target_arch = "wasm32"))]

use aero_storage::{
    StreamingCacheBackend, StreamingDisk, StreamingDiskConfig, DEFAULT_SECTOR_SIZE,
};
use hyper::header::{ACCEPT_RANGES, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_RANGE, ETAG, RANGE};
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Method, Request, Response, Server, StatusCode};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tempfile::tempdir;
use tokio::sync::oneshot;
use url::Url;

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

async fn start_range_server(total_size: u64) -> (Url, Arc<Counters>, oneshot::Sender<()>) {
    let counters = Arc::new(Counters::default());
    let make_svc = {
        let counters = counters.clone();
        make_service_fn(move |_conn| {
            let counters = counters.clone();
            async move {
                Ok::<_, Infallible>(service_fn(move |req| {
                    handle_request(req, total_size, counters.clone())
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
                .insert(ETAG, "\"u64-max\"".parse().unwrap());
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
    let mut buf = vec![0u8; len];
    if end_exclusive == total_size && !buf.is_empty() {
        // Mark the final byte of the image.
        buf[len - 1] = 0xAB;
    }

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
        .insert(ETAG, "\"u64-max\"".parse().unwrap());
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
async fn streaming_disk_handles_u64_max_total_size_without_overflow() {
    let total_size = u64::MAX;
    let (url, counters, shutdown) = start_range_server(total_size).await;

    let cache_dir = tempdir().unwrap();
    let mut config = StreamingDiskConfig::new(url, cache_dir.path());
    config.cache_backend = StreamingCacheBackend::Directory;
    config.options.chunk_size = DEFAULT_SECTOR_SIZE;
    config.options.read_ahead_chunks = 0;
    config.options.max_retries = 1;
    config.options.max_concurrent_fetches = 1;

    let disk = StreamingDisk::open(config).await.unwrap();

    let mut buf = [0u8; 1];
    disk.read_at(total_size - 1, &mut buf).await.unwrap();
    assert_eq!(buf[0], 0xAB);

    let chunk_start = total_size - (total_size % DEFAULT_SECTOR_SIZE);
    let expected = format!("bytes={chunk_start}-{}", total_size - 1);
    let seen = counters
        .last_range_header
        .lock()
        .unwrap()
        .clone()
        .expect("expected at least one range GET");
    assert_eq!(
        seen, expected,
        "Range header should use full 64-bit decimal offsets"
    );

    assert_eq!(counters.head.load(Ordering::SeqCst), 1);
    assert_eq!(counters.range_get.load(Ordering::SeqCst), 1);

    let _ = shutdown.send(());
}
