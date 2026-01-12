#![cfg(not(target_arch = "wasm32"))]

use std::net::SocketAddr;
use std::time::Duration;

use aero_storage_server::{start, StorageServerConfig};
use reqwest::StatusCode;

async fn wait_for_http_ok(client: &reqwest::Client, url: &str) -> anyhow::Result<()> {
    for _ in 0..50 {
        if let Ok(resp) = client.get(url).send().await {
            if resp.status().is_success() {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("endpoint did not become reachable: {url}");
}

async fn wait_for_http_response(client: &reqwest::Client, url: &str) -> anyhow::Result<()> {
    for _ in 0..50 {
        if client.get(url).send().await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("endpoint did not become reachable: {url}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn require_manifest_missing_disables_directory_listing_fallback() -> anyhow::Result<()> {
    let tempdir = tempfile::tempdir()?;
    let server = start(StorageServerConfig {
        bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        images_dir: tempdir.path().to_path_buf(),
        require_manifest: true,
    })
    .await?;

    let base_url = format!("http://{}", server.addr());
    let client = reqwest::Client::new();

    // `healthz` should still report liveness even when not ready.
    wait_for_http_ok(&client, &format!("{base_url}/healthz")).await?;

    // With `require_manifest=true`, `manifest.json` absence should make the server not ready.
    let readyz = format!("{base_url}/readyz");
    wait_for_http_response(&client, &readyz).await?;
    let ready_resp = client.get(&readyz).send().await?;
    assert!(
        !ready_resp.status().is_success(),
        "expected /readyz to be non-2xx when manifest is missing, got {}",
        ready_resp.status()
    );

    let images = client.get(format!("{base_url}/v1/images")).send().await?;
    assert!(
        matches!(
            images.status(),
            StatusCode::INTERNAL_SERVER_ERROR | StatusCode::SERVICE_UNAVAILABLE
        ),
        "expected /v1/images to return 500/503 when manifest is missing, got {}",
        images.status()
    );

    server.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn require_manifest_with_manifest_behaves_normally() -> anyhow::Result<()> {
    let tempdir = tempfile::tempdir()?;

    tokio::fs::write(tempdir.path().join("disk.img"), vec![0u8; 16]).await?;
    tokio::fs::write(
        tempdir.path().join("manifest.json"),
        r#"{
          "images": [
            { "id": "disk", "file": "disk.img", "name": "Disk", "public": true }
          ]
        }"#,
    )
    .await?;

    let server = start(StorageServerConfig {
        bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        images_dir: tempdir.path().to_path_buf(),
        require_manifest: true,
    })
    .await?;

    let base_url = format!("http://{}", server.addr());
    let client = reqwest::Client::new();

    wait_for_http_ok(&client, &format!("{base_url}/readyz")).await?;

    let images = client.get(format!("{base_url}/v1/images")).send().await?;
    assert_eq!(images.status(), StatusCode::OK);

    let body: serde_json::Value = images.json().await?;
    assert_eq!(body.as_array().map(|a| a.len()), Some(1));
    assert_eq!(body[0]["id"], "disk");

    server.shutdown().await?;
    Ok(())
}
