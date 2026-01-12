#![cfg(target_arch = "wasm32")]

// Keep all wasm-only OPFS integration tests in a single crate so `cargo test` on native targets
// doesn't have to link multiple empty test binaries (which can be slow/flaky in resource-limited
// environments).

wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

mod backend {
    use aero_opfs::{DiskError, OpfsStorage};
    use wasm_bindgen_test::wasm_bindgen_test;

    fn unique_path(prefix: &str) -> String {
        let now = js_sys::Date::now() as u64;
        format!("tests/{prefix}-{now}.img")
    }

    fn fill_deterministic(buf: &mut [u8], seed: u32) {
        let mut x = seed;
        for b in buf {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            *b = (x & 0xff) as u8;
        }
    }

    async fn write_sectors(storage: &mut OpfsStorage, lba: u64, buf: &[u8]) {
        match storage {
            OpfsStorage::Sync(backend) => backend.write_sectors(lba, buf).unwrap(),
            OpfsStorage::Async(backend) => backend.write_sectors(lba, buf).await.unwrap(),
            OpfsStorage::IndexedDb(backend) => backend.write_sectors(lba, buf).await.unwrap(),
        }
    }

    async fn read_sectors(storage: &mut OpfsStorage, lba: u64, buf: &mut [u8]) {
        match storage {
            OpfsStorage::Sync(backend) => backend.read_sectors(lba, buf).unwrap(),
            OpfsStorage::Async(backend) => backend.read_sectors(lba, buf).await.unwrap(),
            OpfsStorage::IndexedDb(backend) => backend.read_sectors(lba, buf).await.unwrap(),
        }
    }

    async fn flush(storage: &mut OpfsStorage) {
        match storage {
            OpfsStorage::Sync(backend) => backend.flush().unwrap(),
            OpfsStorage::Async(backend) => backend.flush().await.unwrap(),
            OpfsStorage::IndexedDb(backend) => backend.flush().await.unwrap(),
        }
    }

    #[wasm_bindgen_test(async)]
    async fn opfs_roundtrip_small() {
        let path = unique_path("roundtrip");
        let size = 8 * 1024 * 1024u64;

        let mut backend = match OpfsStorage::open(&path, true, size).await {
            Ok(b) => b,
            Err(DiskError::NotSupported(_)) => return,
            Err(DiskError::BackendUnavailable) => return,
            Err(e) => panic!("open failed: {e:?}"),
        };

        let lba = 7u64;
        let mut write_buf = vec![0u8; 4096];
        fill_deterministic(&mut write_buf, 0x1234_5678);
        write_sectors(&mut backend, lba, &write_buf).await;
        flush(&mut backend).await;

        let mut backend = OpfsStorage::open(&path, false, size).await.unwrap();
        let mut read_buf = vec![0u8; 4096];
        read_sectors(&mut backend, lba, &mut read_buf).await;
        assert_eq!(read_buf, write_buf);
    }

    #[wasm_bindgen_test(async)]
    async fn opfs_large_offset_over_2gb() {
        let path = unique_path("large-offset");
        let size = 2 * 1024 * 1024 * 1024u64 + 16 * 1024 * 1024;

        let mut backend = match OpfsStorage::open(&path, true, size).await {
            Ok(b) => b,
            Err(DiskError::NotSupported(_)) => return,
            Err(DiskError::QuotaExceeded) => return,
            Err(DiskError::BackendUnavailable) => return,
            Err(e) => panic!("open failed: {e:?}"),
        };

        let write_offset = 2 * 1024 * 1024 * 1024u64 + 4 * 1024 * 1024;
        let lba = write_offset / 512;

        let mut write_buf = vec![0u8; 8192];
        fill_deterministic(&mut write_buf, 0xA5A5_5A5A);
        write_sectors(&mut backend, lba, &write_buf).await;
        flush(&mut backend).await;

        let mut backend = OpfsStorage::open(&path, false, size).await.unwrap();
        let mut read_buf = vec![0u8; 8192];
        read_sectors(&mut backend, lba, &mut read_buf).await;
        assert_eq!(read_buf, write_buf);
    }
}

mod aerospar {
    use aero_opfs::{DiskError, OpfsByteStorage};
    use aero_storage::{AeroSparseConfig, AeroSparseDisk, VirtualDisk as _};
    use wasm_bindgen_test::wasm_bindgen_test;

    fn unique_path(prefix: &str) -> String {
        let now = js_sys::Date::now() as u64;
        format!("tests/{prefix}-{now}.aerospar")
    }

    fn fill_deterministic(buf: &mut [u8], seed: u32) {
        let mut x = seed;
        for b in buf {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            *b = (x & 0xff) as u8;
        }
    }

    #[wasm_bindgen_test(async)]
    async fn opfs_aerospar_roundtrip() {
        let path = unique_path("aerospar-roundtrip");

        let storage = match OpfsByteStorage::open(&path, true).await {
            Ok(s) => s,
            Err(DiskError::NotSupported(_)) => return,
            Err(DiskError::QuotaExceeded) => return,
            Err(DiskError::BackendUnavailable) => return,
            Err(e) => panic!("open failed: {e:?}"),
        };

        let mut disk = AeroSparseDisk::create(
            storage,
            AeroSparseConfig {
                disk_size_bytes: 1024 * 1024,
                block_size_bytes: 32 * 1024,
            },
        )
        .unwrap();

        let mut write_buf = vec![0u8; 4096];
        fill_deterministic(&mut write_buf, 0x55AA_1234);
        disk.write_sectors(7, &write_buf).unwrap();
        disk.flush().unwrap();

        let mut storage = disk.into_backend();
        storage.close().unwrap();

        let storage = OpfsByteStorage::open(&path, false).await.unwrap();
        let mut disk = AeroSparseDisk::open(storage).unwrap();
        let mut read_buf = vec![0u8; write_buf.len()];
        disk.read_sectors(7, &mut read_buf).unwrap();
        assert_eq!(read_buf, write_buf);
    }
}

