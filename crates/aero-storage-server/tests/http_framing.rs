#![cfg(not(target_arch = "wasm32"))]

use std::net::SocketAddr;
use std::time::Duration;

use aero_storage_server::{start, StorageServerConfig};
use reqwest::header;

struct TestServer {
    _tempdir: tempfile::TempDir,
    base_url: String,
    server: aero_storage_server::RunningStorageServer,
    client: reqwest::Client,
}

impl TestServer {
    async fn start_with_image(contents: &[u8]) -> anyhow::Result<Self> {
        let tempdir = tempfile::tempdir()?;
        tokio::fs::write(tempdir.path().join("test.img"), contents).await?;

        let server = start(StorageServerConfig {
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            images_dir: tempdir.path().to_path_buf(),
            require_manifest: false,
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
async fn range_response_is_not_chunked_on_the_wire() -> anyhow::Result<()> {
    // Keep the file small: we only care about response framing.
    let file_contents = b"abc";
    let total_len = file_contents.len();

    let server = TestServer::start_with_image(file_contents).await?;

    let resp = server
        .client
        .get(format!("{}/v1/images/test.img", server.base_url))
        .header(header::RANGE, "bytes=0-0")
        .send()
        .await?;

    assert_eq!(resp.version(), reqwest::Version::HTTP_11);
    assert_eq!(resp.status(), reqwest::StatusCode::PARTIAL_CONTENT);

    assert_eq!(
        resp.headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok()),
        Some("1")
    );

    // NOTE: `tower::ServiceExt::oneshot` tests only validate the app-level `Response`, not the
    // actual HTTP/1.1 framing. Hyper may inject `Transfer-Encoding: chunked` at send time, which
    // can cause issues with some intermediaries/CDNs for `206 Partial Content` responses.
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
            "unexpected `Transfer-Encoding` for 206 response: {te:?}"
        );
    }

    let expected_content_range = format!("bytes 0-0/{total_len}");
    assert_eq!(
        resp.headers()
            .get(header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok()),
        Some(expected_content_range.as_str())
    );

    let body = resp.bytes().await?;
    assert_eq!(body.as_ref(), &file_contents[..1]);

    server.shutdown().await?;
    Ok(())
}
