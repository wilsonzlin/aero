use std::net::SocketAddr;
use std::time::Duration;

use aero_storage_server::{start, StorageServerConfig};

struct TestServer {
    _tempdir: tempfile::TempDir,
    base_url: String,
    server: aero_storage_server::RunningStorageServer,
    client: reqwest::Client,
}

impl TestServer {
    async fn start() -> anyhow::Result<Self> {
        let tempdir = tempfile::tempdir()?;
        let server = start(StorageServerConfig {
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            images_dir: tempdir.path().to_path_buf(),
        })
        .await?;

        let base_url = format!("http://{}", server.addr());
        let client = reqwest::Client::new();

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
async fn health_ready_metrics_endpoints_work() -> anyhow::Result<()> {
    let server = TestServer::start().await?;

    let health = server
        .client
        .get(format!("{}/healthz", server.base_url))
        .send()
        .await?;
    assert_eq!(health.status(), reqwest::StatusCode::OK);
    let health_body: serde_json::Value = health.json().await?;
    assert_eq!(health_body["status"], "ok");

    let ready = server
        .client
        .get(format!("{}/readyz", server.base_url))
        .send()
        .await?;
    assert_eq!(ready.status(), reqwest::StatusCode::OK);
    let ready_body: serde_json::Value = ready.json().await?;
    assert_eq!(ready_body["status"], "ok");

    let metrics = server
        .client
        .get(format!("{}/metrics", server.base_url))
        .send()
        .await?;
    assert_eq!(metrics.status(), reqwest::StatusCode::OK);
    assert_eq!(
        metrics
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/plain; version=0.0.4")
    );
    let body = metrics.text().await?;
    assert!(body.contains("aero_storage_server_build_info"));

    server.shutdown().await?;
    Ok(())
}
