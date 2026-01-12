#![cfg(not(target_arch = "wasm32"))]

use std::net::SocketAddr;
use std::sync::Arc;

use aero_storage_server::store::LocalFsImageStore;
use aero_storage_server::AppState;
use reqwest::StatusCode;
use tempfile::TempDir;

async fn spawn_server(image_root: &std::path::Path) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let store = Arc::new(LocalFsImageStore::new(image_root));
    let state = AppState::new(store);
    let app = aero_storage_server::app(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    (addr, handle)
}

#[tokio::test]
async fn list_images_from_manifest() {
    let tmp = TempDir::new().unwrap();
    let image_path = tmp.path().join("win7.img");
    tokio::fs::write(&image_path, vec![0u8; 1234])
        .await
        .unwrap();

    let manifest = r#"{
      "images": [
        {
          "id": "win7",
          "file": "win7.img",
          "name": "Windows 7",
          "description": "Test image",
          "public": true,
          "recommended_chunk_size_bytes": 1048576,
          "content_type": "application/octet-stream"
        }
      ]
    }"#;
    tokio::fs::write(tmp.path().join("manifest.json"), manifest)
        .await
        .unwrap();

    let (addr, handle) = spawn_server(tmp.path()).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("http://{addr}/v1/images"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(reqwest::header::CACHE_CONTROL)
            .unwrap()
            .to_str()
            .unwrap(),
        "no-cache"
    );

    let images: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(images.len(), 1);
    assert_eq!(images[0]["id"], "win7");
    assert_eq!(images[0]["name"], "Windows 7");
    assert_eq!(images[0]["size_bytes"], 1234);
    assert_eq!(images[0]["recommended_chunk_size_bytes"], 1048576);

    handle.abort();
}

#[tokio::test]
async fn get_image_meta_has_correct_size() {
    let tmp = TempDir::new().unwrap();
    let image_path = tmp.path().join("disk.img");
    tokio::fs::write(&image_path, vec![0u8; 99]).await.unwrap();

    tokio::fs::write(
        tmp.path().join("manifest.json"),
        r#"{
          "images": [
            { "id": "disk", "file": "disk.img", "name": "Disk", "public": true }
          ]
        }"#,
    )
    .await
    .unwrap();

    let (addr, handle) = spawn_server(tmp.path()).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("http://{addr}/v1/images/disk/meta"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let meta: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(meta["id"], "disk");
    assert_eq!(meta["size_bytes"], 99);

    handle.abort();
}

#[tokio::test]
async fn overly_long_image_id_meta_is_rejected_with_400() {
    let tmp = TempDir::new().unwrap();

    // > `MAX_IMAGE_ID_LEN` should be rejected by the store validator even if a file exists.
    let long_id = "a".repeat(aero_storage_server::store::MAX_IMAGE_ID_LEN + 1);
    tokio::fs::write(tmp.path().join(&long_id), vec![0u8; 1])
        .await
        .unwrap();

    let (addr, handle) = spawn_server(tmp.path()).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("http://{addr}/v1/images/{long_id}/meta"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    handle.abort();
}
