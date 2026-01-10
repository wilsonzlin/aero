use std::{env, net::SocketAddr, path::PathBuf, sync::Arc};

use aero_storage_server::{http, store::LocalFsImageStore};
use axum::{routing::get, Router};

#[tokio::main]
async fn main() {
    let listen_addr: SocketAddr = env::var("AERO_STORAGE_LISTEN_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8080".to_string())
        .parse()
        .expect("AERO_STORAGE_LISTEN_ADDR must be a valid host:port socket address");

    let image_root = PathBuf::from(
        env::var("AERO_STORAGE_IMAGE_ROOT").unwrap_or_else(|_| "./images".to_string()),
    );

    let store = Arc::new(LocalFsImageStore::new(&image_root));

    let app = Router::new()
        .route("/healthz", get(|| async { "ok\n" }))
        // Disk image streaming endpoint (Range + CORS).
        //
        // See `crates/aero-storage-server/src/http/images.rs` for the full behavior.
        .merge(http::images::router(store));

    eprintln!(
        "aero-storage-server listening on http://{listen_addr} (root: {})",
        image_root.display()
    );

    let listener = tokio::net::TcpListener::bind(listen_addr)
        .await
        .expect("failed to bind listen address");

    axum::serve(listener, app)
        .await
        .expect("server exited unexpectedly");
}
