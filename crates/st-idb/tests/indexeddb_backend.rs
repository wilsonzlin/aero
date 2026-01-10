use st_idb::io::storage::backends::indexeddb::{IndexedDbBackend, IndexedDbBackendOptions};
use st_idb::io::storage::DiskBackend;
use wasm_bindgen_test::wasm_bindgen_test;

wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_worker);

fn unique_db_name(prefix: &str) -> String {
    let now = js_sys::Date::now() as u64;
    let rand = (js_sys::Math::random() * 1_000_000.0) as u64;
    format!("{prefix}-{now:x}-{rand:x}")
}

#[wasm_bindgen_test(async)]
async fn persistence_across_reopen() {
    let db_name = unique_db_name("st-idb-persist");
    let _ = IndexedDbBackend::delete_database(&db_name).await;

    let capacity = 8 * 1024 * 1024;
    let mut backend =
        IndexedDbBackend::open(&db_name, capacity, IndexedDbBackendOptions::default())
            .await
            .unwrap();

    backend.write_at(0, b"hello world").await.unwrap();
    backend.flush().await.unwrap();
    drop(backend);

    let mut backend2 =
        IndexedDbBackend::open(&db_name, capacity, IndexedDbBackendOptions::default())
            .await
            .unwrap();
    let mut buf = vec![0u8; 11];
    backend2.read_at(0, &mut buf).await.unwrap();
    assert_eq!(&buf, b"hello world");

    IndexedDbBackend::delete_database(&db_name).await.unwrap();
}

#[wasm_bindgen_test(async)]
async fn eviction_round_trip() {
    let db_name = unique_db_name("st-idb-evict");
    let _ = IndexedDbBackend::delete_database(&db_name).await;

    let capacity = 8 * 1024 * 1024;
    let block_size = 1024 * 1024;
    let mut backend = IndexedDbBackend::open(
        &db_name,
        capacity,
        IndexedDbBackendOptions {
            max_resident_bytes: 2 * block_size,
            flush_chunk_blocks: 2,
        },
    )
    .await
    .unwrap();

    backend.write_at(0, &[0xAA; 4]).await.unwrap();
    backend.flush().await.unwrap();

    backend
        .write_at(block_size as u64, &[0xBB; 4])
        .await
        .unwrap();
    backend.flush().await.unwrap();

    // Writing a third block forces eviction (cache holds only 2 blocks).
    backend
        .write_at(2 * block_size as u64, &[0xCC; 4])
        .await
        .unwrap();
    backend.flush().await.unwrap();

    let mut buf = vec![0u8; 4];
    backend.read_at(0, &mut buf).await.unwrap();
    assert_eq!(&buf, &[0xAA; 4]);

    IndexedDbBackend::delete_database(&db_name).await.unwrap();
}

#[wasm_bindgen_test(async)]
async fn crash_safety_simulation_unflushed_write_lost() {
    let db_name = unique_db_name("st-idb-crash");
    let _ = IndexedDbBackend::delete_database(&db_name).await;

    let capacity = 8 * 1024 * 1024;
    let mut backend =
        IndexedDbBackend::open(&db_name, capacity, IndexedDbBackendOptions::default())
            .await
            .unwrap();

    backend.write_at(0, b"old").await.unwrap();
    backend.flush().await.unwrap();

    backend.write_at(0, b"new").await.unwrap();
    // Intentionally skip flush() to simulate crash.
    drop(backend);

    let mut backend2 =
        IndexedDbBackend::open(&db_name, capacity, IndexedDbBackendOptions::default())
            .await
            .unwrap();
    let mut buf = vec![0u8; 3];
    backend2.read_at(0, &mut buf).await.unwrap();
    assert_eq!(&buf, b"old");

    backend2.write_at(0, b"new").await.unwrap();
    backend2.flush().await.unwrap();
    drop(backend2);

    let mut backend3 =
        IndexedDbBackend::open(&db_name, capacity, IndexedDbBackendOptions::default())
            .await
            .unwrap();
    let mut buf2 = vec![0u8; 3];
    backend3.read_at(0, &mut buf2).await.unwrap();
    assert_eq!(&buf2, b"new");

    IndexedDbBackend::delete_database(&db_name).await.unwrap();
}
