use aero_io_snapshot::io::state::{IoSnapshot, SnapshotError, SnapshotWriter};
use aero_io_snapshot::io::storage::state::{
    DiskBackendState, DiskLayerState, LocalDiskBackendKind, LocalDiskBackendState,
    NvmeControllerState,
};

#[test]
fn nvme_snapshot_rejects_excessive_io_queue_count() {
    const TAG_IO_QUEUES: u16 = 3;

    let mut w = SnapshotWriter::new(
        NvmeControllerState::DEVICE_ID,
        NvmeControllerState::DEVICE_VERSION,
    );
    w.field_bytes(TAG_IO_QUEUES, u32::MAX.to_le_bytes().to_vec());

    let mut state = NvmeControllerState::default();
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject excessive IO queue count");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("nvme io queue count")
    );
}

#[test]
fn nvme_snapshot_rejects_excessive_in_flight_command_count() {
    const TAG_IN_FLIGHT: u16 = 4;

    let mut w = SnapshotWriter::new(
        NvmeControllerState::DEVICE_ID,
        NvmeControllerState::DEVICE_VERSION,
    );
    w.field_bytes(TAG_IN_FLIGHT, u32::MAX.to_le_bytes().to_vec());

    let mut state = NvmeControllerState::default();
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject excessive in-flight command count");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("nvme in_flight count")
    );
}

#[test]
fn disk_backend_state_rejects_excessive_string_length() {
    // Keep in sync with `MAX_DISK_STRING_BYTES` in `aero-io-snapshot`.
    const MAX_LEN: u32 = 64 * 1024;

    // local backend, opfs kind, then an oversized key string.
    let mut bytes = Vec::new();
    bytes.push(0); // kind = local
    bytes.push(0); // backend_kind = opfs
    bytes.extend_from_slice(&(MAX_LEN + 1).to_le_bytes());

    let err = DiskBackendState::decode(&bytes).expect_err("should reject oversized string");
    assert_eq!(err, SnapshotError::InvalidFieldEncoding("string too long"));
}

#[test]
fn disk_backend_state_rejects_invalid_overlay_block_size() {
    let mut bytes = Vec::new();
    bytes.push(0); // kind = local
    bytes.push(0); // backend_kind = opfs

    // key = "k"
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(b"k");

    // overlay present
    bytes.push(1);

    // overlay.file_name = "o"
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(b"o");
    // overlay.disk_size_bytes = 4096
    bytes.extend_from_slice(&4096u64.to_le_bytes());
    // overlay.block_size_bytes = 0 (invalid)
    bytes.extend_from_slice(&0u32.to_le_bytes());

    let err =
        DiskBackendState::decode(&bytes).expect_err("should reject invalid overlay block size");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("overlay block_size")
    );
}

#[test]
fn disk_backend_state_rejects_zero_remote_chunk_size() {
    // remote backend with chunk_size=0 should fail before decoding overlay/cache.
    let mut bytes = Vec::new();
    bytes.push(1); // kind = remote

    // image_id = "i"
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(b"i");
    // version = "v"
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(b"v");
    // delivery_type = "r"
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(b"r");

    bytes.push(0); // validator_kind = none
    bytes.extend_from_slice(&0u32.to_le_bytes()); // chunk_size = 0

    let err = DiskBackendState::decode(&bytes).expect_err("should reject chunk_size=0");
    assert_eq!(err, SnapshotError::InvalidFieldEncoding("chunk_size"));
}

#[test]
fn disk_layer_snapshot_rejects_invalid_sector_size() {
    const TAG_SECTOR_SIZE: u16 = 2;
    const TAG_SIZE_BYTES: u16 = 3;
    const TAG_BACKEND_STATE: u16 = 8;

    let backend = DiskBackendState::Local(LocalDiskBackendState {
        kind: LocalDiskBackendKind::Other,
        key: "disk0".to_string(),
        overlay: None,
    });

    let mut w = SnapshotWriter::new(DiskLayerState::DEVICE_ID, DiskLayerState::DEVICE_VERSION);
    w.field_bytes(TAG_BACKEND_STATE, backend.encode());
    w.field_u32(TAG_SECTOR_SIZE, 1);
    w.field_u64(TAG_SIZE_BYTES, 4096);

    let mut state = DiskLayerState::new(backend, 4096, 512);
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject invalid sector size");
    assert_eq!(err, SnapshotError::InvalidFieldEncoding("sector_size"));
}

#[test]
fn disk_layer_snapshot_rejects_unaligned_disk_size() {
    const TAG_SECTOR_SIZE: u16 = 2;
    const TAG_SIZE_BYTES: u16 = 3;
    const TAG_BACKEND_STATE: u16 = 8;

    let backend = DiskBackendState::Local(LocalDiskBackendState {
        kind: LocalDiskBackendKind::Other,
        key: "disk0".to_string(),
        overlay: None,
    });

    let mut w = SnapshotWriter::new(DiskLayerState::DEVICE_ID, DiskLayerState::DEVICE_VERSION);
    w.field_bytes(TAG_BACKEND_STATE, backend.encode());
    w.field_u32(TAG_SECTOR_SIZE, 512);
    w.field_u64(TAG_SIZE_BYTES, 513);

    let mut state = DiskLayerState::new(backend, 4096, 512);
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject disk sizes that aren't sector-aligned");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("disk_size not multiple of sector_size")
    );
}
