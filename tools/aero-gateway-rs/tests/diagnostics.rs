use std::{path::PathBuf, time::SystemTime};

use aero_gateway_rs::{
    build_app,
    capture::{CaptureConfig, CaptureManager, ConnectionMeta, Direction},
    GatewayConfig,
};
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use sha2::{Digest, Sha256};
use tower::ServiceExt;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_endpoints_require_api_key() {
    let app = build_app(GatewayConfig {
        admin_api_key: Some("topsecret".to_string()),
        capture: None,
    })
    .await
    .unwrap();

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/stats")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/stats")
                .header("ADMIN_API_KEY", "wrong")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/admin/stats")
                .header("ADMIN_API_KEY", "topsecret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(payload.get("active_tcp_connections").is_some());
    assert!(payload.get("bytes_total").is_some());
    assert!(payload.get("uptime_seconds").is_some());
    assert!(payload.get("version").is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capture_writes_files_without_session_secret_and_enforces_limits() {
    let tmp = tempfile::tempdir().unwrap();
    let capture_dir = tmp.path().to_path_buf();

    let manager = CaptureManager::new(Some(CaptureConfig {
        dir: capture_dir.clone(),
        max_bytes: 5_000,
        max_files: 2,
    }))
    .await
    .unwrap();

    let session_secret = "supersecret-session-token";
    let expected_hash = hex::encode(Sha256::digest(session_secret.as_bytes()));

    // Write 3 capture files; only 2 should remain due to CAPTURE_MAX_FILES.
    for connection_id in 1..=3 {
        let capture = manager
            .open_connection_capture(ConnectionMeta {
                connection_id,
                started_at: SystemTime::now(),
                client_ip: Some("127.0.0.1".parse().unwrap()),
                session_secret: Some(session_secret),
                target: "example.com:80",
            })
            .await
            .unwrap()
            .expect("capture should be enabled");

        let payload = vec![0x42u8; 2048];
        capture
            .record(Direction::ClientToTarget, &payload)
            .await
            .unwrap();
        capture.close().await.unwrap();
    }

    let jsonl_files = list_jsonl_files(&capture_dir);
    assert!(
        jsonl_files.len() <= 2,
        "expected CAPTURE_MAX_FILES to prune old captures; found {:?}",
        jsonl_files
    );

    let total_bytes: u64 = jsonl_files
        .iter()
        .map(|path| std::fs::metadata(path).unwrap().len())
        .sum();
    assert!(
        total_bytes <= 5_000,
        "expected CAPTURE_MAX_BYTES to prune old captures, total_bytes={total_bytes}"
    );

    // Validate metadata + ensure the raw session secret is not written to disk.
    let newest = jsonl_files
        .last()
        .expect("expected at least one capture file");
    let data = std::fs::read_to_string(newest).unwrap();
    assert!(
        !data.contains(session_secret),
        "capture file should not contain the raw session secret"
    );

    let mut lines = data.lines();
    let meta_line = lines.next().expect("missing meta line");
    let meta: serde_json::Value = serde_json::from_str(meta_line).unwrap();
    assert_eq!(meta["type"], "meta");
    assert_eq!(meta["session_hash"], expected_hash);

    let chunk_line = lines.next().expect("missing chunk line");
    let chunk: serde_json::Value = serde_json::from_str(chunk_line).unwrap();
    assert_eq!(chunk["type"], "chunk");
    assert_eq!(chunk["direction"], "client_to_target");
    assert_eq!(chunk["len"], 2048);
}

fn list_jsonl_files(dir: &PathBuf) -> Vec<PathBuf> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            (path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")).then_some(path)
        })
        .collect();
    files.sort();
    files
}
