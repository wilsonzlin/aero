use st_idb::io::storage::backends::indexeddb::{IndexedDbBackend, IndexedDbBackendOptions};
use st_idb::io::storage::DiskBackend;
use st_idb::platform::storage::indexeddb as idb;
use st_idb::StorageError;
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

    let mut backend2 = IndexedDbBackend::open_existing(&db_name, IndexedDbBackendOptions::default())
        .await
        .unwrap();
    assert_eq!(backend2.capacity(), capacity);
    let mut buf = vec![0u8; 11];
    backend2.read_at(0, &mut buf).await.unwrap();
    assert_eq!(&buf, b"hello world");

    IndexedDbBackend::delete_database(&db_name).await.unwrap();
}

#[wasm_bindgen_test(async)]
async fn non_default_block_size_persists_and_open_existing_works() {
    let db_name = unique_db_name("st-idb-block-size");
    let _ = IndexedDbBackend::delete_database(&db_name).await;

    let capacity = 8 * 1024 * 1024;
    let block_size = 64 * 1024;

    let mut backend = IndexedDbBackend::create(
        &db_name,
        capacity,
        block_size,
        IndexedDbBackendOptions::default(),
    )
    .await
    .unwrap();

    assert_eq!(backend.block_size(), block_size);

    // Write across a block boundary to ensure the on-disk block size is respected.
    backend
        .write_at((block_size - 3) as u64, b"abcdef")
        .await
        .unwrap();
    backend.flush().await.unwrap();
    drop(backend);

    let mut backend2 = IndexedDbBackend::open_existing(&db_name, IndexedDbBackendOptions::default())
        .await
        .unwrap();
    assert_eq!(backend2.capacity(), capacity);
    assert_eq!(backend2.block_size(), block_size);

    let mut buf = vec![0u8; 6];
    backend2
        .read_at((block_size - 3) as u64, &mut buf)
        .await
        .unwrap();
    assert_eq!(&buf, b"abcdef");

    IndexedDbBackend::delete_database(&db_name).await.unwrap();
}

#[wasm_bindgen_test(async)]
async fn open_existing_rejects_missing_meta_keys() {
    let db_name = unique_db_name("st-idb-missing-meta");
    let _ = IndexedDbBackend::delete_database(&db_name).await;

    // Create a DB with the expected schema but without any metadata entries.
    let db = idb::open_database_with_schema(&db_name, 1, |db, old, _new| {
        if old < 1 {
            db.create_object_store("meta")?;
            db.create_object_store("blocks")?;
        }
        Ok(())
    })
    .await
    .unwrap();
    db.close();

    let err = IndexedDbBackend::open_existing(&db_name, IndexedDbBackendOptions::default())
        .await
        .err()
        .expect("expected open_existing to fail for missing meta");
    assert!(matches!(err, StorageError::MissingMeta));

    // Add only one meta key and ensure we still reject it as corrupt/incomplete.
    let db = idb::open_database_with_schema(&db_name, 1, |_db, _old, _new| Ok(()))
        .await
        .unwrap();
    idb::put_string(&db, "meta", "format_version", "1")
        .await
        .unwrap();
    db.close();

    let err = IndexedDbBackend::open_existing(&db_name, IndexedDbBackendOptions::default())
        .await
        .err()
        .expect("expected open_existing to fail for missing meta keys");
    assert!(matches!(err, StorageError::Corrupt("missing block_size")));

    IndexedDbBackend::delete_database(&db_name).await.unwrap();
}

#[wasm_bindgen_test(async)]
async fn unwritten_blocks_read_as_zero() {
    let db_name = unique_db_name("st-idb-zeros");
    let _ = IndexedDbBackend::delete_database(&db_name).await;

    let capacity = 8 * 1024 * 1024;
    let mut backend =
        IndexedDbBackend::open(&db_name, capacity, IndexedDbBackendOptions::default())
            .await
            .unwrap();

    let mut buf = vec![0xAA; 32];
    backend.read_at(0, &mut buf).await.unwrap();
    assert_eq!(buf, vec![0u8; 32]);

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
async fn eviction_writeback_persists_without_explicit_flush() {
    let db_name = unique_db_name("st-idb-evict-writeback");
    let _ = IndexedDbBackend::delete_database(&db_name).await;

    let capacity = 8 * 1024 * 1024;
    let block_size = 1024 * 1024;

    let mut backend = IndexedDbBackend::open(
        &db_name,
        capacity,
        IndexedDbBackendOptions {
            // Cache holds one 1MiB block, so writing another block forces eviction.
            max_resident_bytes: block_size,
            flush_chunk_blocks: 1,
        },
    )
    .await
    .unwrap();

    backend.write_at(0, b"persist me").await.unwrap();
    // Force eviction of block 0. The evicted dirty block should be persisted.
    backend.write_at(block_size as u64, b"other").await.unwrap();
    drop(backend);

    let mut backend2 = IndexedDbBackend::open_existing(&db_name, IndexedDbBackendOptions::default())
        .await
        .unwrap();
    let mut buf = vec![0u8; 10];
    backend2.read_at(0, &mut buf).await.unwrap();
    assert_eq!(&buf, b"persist me");

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

    let mut backend2 = IndexedDbBackend::open_existing(&db_name, IndexedDbBackendOptions::default())
        .await
        .unwrap();
    let mut buf = vec![0u8; 3];
    backend2.read_at(0, &mut buf).await.unwrap();
    assert_eq!(&buf, b"old");

    backend2.write_at(0, b"new").await.unwrap();
    backend2.flush().await.unwrap();
    drop(backend2);

    let mut backend3 = IndexedDbBackend::open_existing(&db_name, IndexedDbBackendOptions::default())
        .await
        .unwrap();
    let mut buf2 = vec![0u8; 3];
    backend3.read_at(0, &mut buf2).await.unwrap();
    assert_eq!(&buf2, b"new");

    IndexedDbBackend::delete_database(&db_name).await.unwrap();
}

#[wasm_bindgen_test(async)]
async fn clear_blocks_resets_persisted_data_and_cache() {
    let db_name = unique_db_name("st-idb-clear-blocks");
    let _ = IndexedDbBackend::delete_database(&db_name).await;

    let capacity = 8 * 1024 * 1024;
    let mut backend =
        IndexedDbBackend::open(&db_name, capacity, IndexedDbBackendOptions::default())
            .await
            .unwrap();

    backend.write_at(0, b"hello world").await.unwrap();
    backend.flush().await.unwrap();

    // Sanity: read back the data (may hit cache).
    let mut buf = vec![0u8; 11];
    backend.read_at(0, &mut buf).await.unwrap();
    assert_eq!(&buf, b"hello world");

    backend.clear_blocks().await.unwrap();

    // Reads should observe an all-zero disk (no stale cache).
    let mut cleared = vec![0xAA; 11];
    backend.read_at(0, &mut cleared).await.unwrap();
    assert_eq!(cleared, vec![0u8; 11]);

    // And the clearing should persist across re-open.
    drop(backend);
    let mut backend2 =
        IndexedDbBackend::open(&db_name, capacity, IndexedDbBackendOptions::default())
            .await
            .unwrap();
    let mut cleared2 = vec![0xAA; 11];
    backend2.read_at(0, &mut cleared2).await.unwrap();
    assert_eq!(cleared2, vec![0u8; 11]);

    IndexedDbBackend::delete_database(&db_name).await.unwrap();
}
