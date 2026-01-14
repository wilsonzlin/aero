#![cfg(not(target_arch = "wasm32"))]

use std::net::SocketAddr;
use std::time::Duration;

use aero_storage_server::{start, RunningStorageServer, StorageServerConfig};
use reqwest::header;

struct TestServer {
    _tempdir: tempfile::TempDir,
    base_url: String,
    server: RunningStorageServer,
    client: reqwest::Client,
}

impl TestServer {
    async fn start_with_chunked_fixture() -> anyhow::Result<Self> {
        let tempdir = tempfile::tempdir()?;

        tokio::fs::write(tempdir.path().join("disk.img"), b"raw image bytes").await?;

        // Catalog manifest so `get_image` succeeds and marks the image public.
        let catalog = serde_json::json!({
            "images": [
                { "id": "disk", "file": "disk.img", "name": "Disk", "public": true }
            ]
        })
        .to_string();
        tokio::fs::write(tempdir.path().join("manifest.json"), catalog).await?;

        let chunk_root = tempdir.path().join("chunked/disk/v1/chunks");
        tokio::fs::create_dir_all(&chunk_root).await?;

        // Keep manifest small: we only care about response framing.
        let manifest = serde_json::json!({
            "schema": "aero.chunked-disk-image.v1",
            "imageId": "disk",
            "version": "v1",
            "mimeType": "application/octet-stream",
            "totalSize": 1,
            "chunkSize": 1,
            "chunkCount": 1,
            "chunkIndexWidth": 8,
            "chunks": [{ "size": 1 }]
        })
        .to_string();
        tokio::fs::write(
            tempdir.path().join("chunked/disk/v1/manifest.json"),
            manifest.as_bytes(),
        )
        .await?;
        tokio::fs::write(chunk_root.join("00000000.bin"), b"x").await?;

        let server = start(StorageServerConfig {
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            images_dir: tempdir.path().to_path_buf(),
            require_manifest: true,
        })
        .await?;

        let base_url = format!("http://{}", server.addr());
        let client = reqwest::Client::builder().http1_only().build()?;

        // Ensure the server is accepting connections before returning.
        let ready_url = format!("{}/readyz", base_url);
        let mut ready = false;
        for _ in 0..50 {
            if let Ok(resp) = client.get(&ready_url).send().await {
                if resp.status().is_success() {
                    ready = true;
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        anyhow::ensure!(ready, "/ready endpoint did not become ready in time");

        Ok(Self {
            _tempdir: tempdir,
            base_url,
            server,
            client,
        })
    }

    async fn shutdown(self) -> anyhow::Result<()> {
        self.server.shutdown().await
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chunked_responses_are_not_chunked_transfer_encoding_on_http1() -> anyhow::Result<()> {
    let server = TestServer::start_with_chunked_fixture().await?;

    for path in [
        "/v1/images/disk/chunked/v1/manifest.json",
        "/v1/images/disk/chunked/v1/chunks/00000000.bin",
    ] {
        let resp = server
            .client
            .get(format!("{}{}", server.base_url, path))
            .send()
            .await?;

        assert_eq!(resp.version(), reqwest::Version::HTTP_11);
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        assert!(
            resp.headers().get(header::CONTENT_LENGTH).is_some(),
            "{path}: expected Content-Length"
        );

        // Ensure Hyper didn't fall back to HTTP/1.1 chunked framing.
        if let Some(te) = resp
            .headers()
            .get(header::TRANSFER_ENCODING)
            .and_then(|v| v.to_str().ok())
        {
            let has_chunked = te
                .split(',')
                .map(|s| s.trim().to_ascii_lowercase())
                .any(|s| s == "chunked");
            assert!(
                !has_chunked,
                "{path}: unexpected Transfer-Encoding for chunked image response: {te:?}"
            );
        }
    }

    server.shutdown().await?;
    Ok(())
}
