#![cfg(not(target_arch = "wasm32"))]

use std::{
    future::Future,
    net::SocketAddr,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use aero_storage_server::{
    store::{BoxedAsyncRead, ImageCatalogEntry, ImageMeta, ImageStore, StoreError},
    AppState,
};
use reqwest::StatusCode;
use tokio::{
    io::{AsyncRead, ReadBuf},
    sync::oneshot,
};

struct BlockingReader {
    release: Option<oneshot::Receiver<()>>,
    sent: bool,
}

impl AsyncRead for BlockingReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if let Some(release) = self.release.as_mut() {
            match Pin::new(release).poll(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(_) => {
                    self.release = None;
                }
            }
        }

        if self.sent {
            return Poll::Ready(Ok(()));
        }

        if buf.remaining() > 0 {
            buf.put_slice(&[0u8]);
            self.sent = true;
        }
        Poll::Ready(Ok(()))
    }
}

struct BlockingImageStore {
    entry: ImageCatalogEntry,
    open_tx: Mutex<Option<oneshot::Sender<()>>>,
    release_rx: Mutex<Option<oneshot::Receiver<()>>>,
}

#[async_trait::async_trait]
impl ImageStore for BlockingImageStore {
    async fn list_images(&self) -> Result<Vec<ImageCatalogEntry>, StoreError> {
        Ok(vec![self.entry.clone()])
    }

    async fn get_image(&self, image_id: &str) -> Result<ImageCatalogEntry, StoreError> {
        if image_id == self.entry.id {
            Ok(self.entry.clone())
        } else {
            Err(StoreError::NotFound)
        }
    }

    async fn get_meta(&self, image_id: &str) -> Result<ImageMeta, StoreError> {
        Ok(self.get_image(image_id).await?.meta)
    }

    async fn open_range(
        &self,
        _image_id: &str,
        _start: u64,
        _len: u64,
    ) -> Result<BoxedAsyncRead, StoreError> {
        if let Some(tx) = self.open_tx.lock().unwrap().take() {
            let _ = tx.send(());
        }

        let release = self.release_rx.lock().unwrap().take();
        Ok(Box::pin(BlockingReader {
            release,
            sent: false,
        }))
    }
}

async fn spawn_server(app: axum::Router) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    (addr, handle)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bytes_concurrency_limit_rejects_second_in_flight_request() {
    let (open_tx, open_rx) = oneshot::channel::<()>();
    let (release_tx, release_rx) = oneshot::channel::<()>();

    let store = Arc::new(BlockingImageStore {
        entry: ImageCatalogEntry {
            id: "disk".to_string(),
            name: "disk".to_string(),
            description: None,
            recommended_chunk_size_bytes: None,
            public: true,
            meta: ImageMeta {
                size: 1,
                etag: None,
                last_modified: None,
                content_type: aero_storage_server::store::CONTENT_TYPE_DISK_IMAGE,
            },
        },
        open_tx: Mutex::new(Some(open_tx)),
        release_rx: Mutex::new(Some(release_rx)),
    });

    let state = AppState::new(store).with_max_concurrent_bytes_requests(1);
    let app = aero_storage_server::app(state);
    let (addr, handle) = spawn_server(app).await;

    let client = reqwest::Client::new();
    let url = format!("http://{addr}/v1/images/disk");

    let resp1 = client.get(&url).send().await.unwrap();
    assert_eq!(resp1.status(), StatusCode::OK);

    // Ensure the first request has reached `open_range` and is now holding its semaphore permit.
    open_rx.await.unwrap();

    let resp2 = client.get(&url).send().await.unwrap();
    assert_eq!(resp2.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        resp2.headers()
            .get(reqwest::header::CACHE_CONTROL)
            .unwrap()
            .to_str()
            .unwrap(),
        "no-store, no-transform"
    );
    assert_eq!(
        resp2.headers()
            .get("access-control-allow-origin")
            .unwrap()
            .to_str()
            .unwrap(),
        "*"
    );

    // Let the first request finish cleanly.
    let _ = release_tx.send(());
    let body = resp1.bytes().await.unwrap();
    assert_eq!(body.len(), 1);

    handle.abort();
}
