#![cfg(target_arch = "wasm32")]

use aero_opfs::OpfsByteStorage;
use aero_opfs::DiskError;
use aero_storage::{AeroSparseConfig, AeroSparseDisk, VirtualDisk};
use wasm_bindgen_test::wasm_bindgen_test;

wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

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
