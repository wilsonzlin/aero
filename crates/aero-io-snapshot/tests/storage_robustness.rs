use aero_io_snapshot::io::state::codec::Encoder;
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotError, SnapshotWriter};
use aero_io_snapshot::io::storage::state::{
    AhciControllerState, DiskBackendState, DiskLayerState, IdeControllerState,
    LocalDiskBackendKind, LocalDiskBackendState, NvmeControllerState, MAX_IDE_DATA_BUFFER_BYTES,
};
use aero_storage::SECTOR_SIZE;

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
fn nvme_snapshot_rejects_admin_sq_size_zero() {
    const TAG_ADMIN_SQ: u16 = 5;

    let admin_sq = Encoder::new()
        .u16(0) // qid
        .u64(0) // base
        .u16(0) // size (invalid)
        .u16(0) // head
        .u16(0) // tail
        .u16(0) // cqid
        .finish();

    let mut w = SnapshotWriter::new(
        NvmeControllerState::DEVICE_ID,
        NvmeControllerState::DEVICE_VERSION,
    );
    w.field_bytes(TAG_ADMIN_SQ, admin_sq);

    let mut state = NvmeControllerState::default();
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject admin SQ size 0");
    assert_eq!(err, SnapshotError::InvalidFieldEncoding("nvme sq size"));
}

#[test]
fn nvme_snapshot_rejects_admin_cq_size_zero() {
    const TAG_ADMIN_CQ: u16 = 6;

    let admin_cq = Encoder::new()
        .u16(0) // qid
        .u64(0) // base
        .u16(0) // size (invalid)
        .u16(0) // head
        .u16(0) // tail
        .bool(true) // phase
        .bool(true) // irq_enabled
        .finish();

    let mut w = SnapshotWriter::new(
        NvmeControllerState::DEVICE_ID,
        NvmeControllerState::DEVICE_VERSION,
    );
    w.field_bytes(TAG_ADMIN_CQ, admin_cq);

    let mut state = NvmeControllerState::default();
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject admin CQ size 0");
    assert_eq!(err, SnapshotError::InvalidFieldEncoding("nvme cq size"));
}

#[test]
fn nvme_snapshot_rejects_admin_sq_head_out_of_bounds() {
    const TAG_ADMIN_SQ: u16 = 5;

    let admin_sq = Encoder::new()
        .u16(0) // qid
        .u64(0) // base
        .u16(1) // size
        .u16(1) // head (invalid; must be < size)
        .u16(0) // tail
        .u16(0) // cqid
        .finish();

    let mut w = SnapshotWriter::new(
        NvmeControllerState::DEVICE_ID,
        NvmeControllerState::DEVICE_VERSION,
    );
    w.field_bytes(TAG_ADMIN_SQ, admin_sq);

    let mut state = NvmeControllerState::default();
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject out-of-bounds admin SQ head");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("nvme sq head/tail")
    );
}

#[test]
fn nvme_snapshot_rejects_admin_cq_tail_out_of_bounds() {
    const TAG_ADMIN_CQ: u16 = 6;

    let admin_cq = Encoder::new()
        .u16(0) // qid
        .u64(0) // base
        .u16(1) // size
        .u16(0) // head
        .u16(1) // tail (invalid; must be < size)
        .bool(true) // phase
        .bool(true) // irq_enabled
        .finish();

    let mut w = SnapshotWriter::new(
        NvmeControllerState::DEVICE_ID,
        NvmeControllerState::DEVICE_VERSION,
    );
    w.field_bytes(TAG_ADMIN_CQ, admin_cq);

    let mut state = NvmeControllerState::default();
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject out-of-bounds admin CQ tail");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("nvme cq head/tail")
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
fn disk_backend_state_rejects_invalid_backend_kind() {
    // backend kind is a single byte; only 0 (local) and 1 (remote) are valid.
    let bytes = vec![2];
    let err = DiskBackendState::decode(&bytes).expect_err("should reject invalid backend kind");
    assert_eq!(err, SnapshotError::InvalidFieldEncoding("backend kind"));
}

#[test]
fn disk_backend_state_rejects_invalid_utf8_string() {
    // local backend, opfs kind, key length=1, key byte is invalid utf-8.
    let mut bytes = Vec::new();
    bytes.push(0); // kind = local
    bytes.push(0); // backend_kind = opfs
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.push(0xff);

    let err = DiskBackendState::decode(&bytes).expect_err("should reject invalid utf8");
    assert_eq!(err, SnapshotError::InvalidFieldEncoding("string utf8"));
}

#[test]
fn disk_backend_state_rejects_overlay_disk_size_zero() {
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
    // overlay.disk_size_bytes = 0 (invalid)
    bytes.extend_from_slice(&0u64.to_le_bytes());
    // overlay.block_size_bytes (valid, but should not be reached)
    bytes.extend_from_slice(&512u32.to_le_bytes());

    let err = DiskBackendState::decode(&bytes).expect_err("should reject overlay disk_size=0");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("overlay disk_size")
    );
}

#[test]
fn disk_backend_state_rejects_overlay_disk_size_unaligned() {
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
    // overlay.disk_size_bytes = 513 (invalid; not multiple of 512)
    bytes.extend_from_slice(&513u64.to_le_bytes());
    // overlay.block_size_bytes
    bytes.extend_from_slice(&512u32.to_le_bytes());

    let err =
        DiskBackendState::decode(&bytes).expect_err("should reject unaligned overlay disk_size");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("overlay disk_size not multiple of 512")
    );
}

#[test]
fn disk_backend_state_rejects_overlay_block_size_not_multiple_of_512() {
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
    // overlay.block_size_bytes = 513 (invalid; not multiple of 512)
    bytes.extend_from_slice(&513u32.to_le_bytes());

    let err = DiskBackendState::decode(&bytes)
        .expect_err("should reject overlay block_size not multiple of 512");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("overlay block_size not multiple of 512")
    );
}

#[test]
fn disk_backend_state_rejects_overlay_block_size_not_power_of_two() {
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
    // overlay.block_size_bytes = 1536 (multiple of 512, but not power-of-two)
    bytes.extend_from_slice(&1536u32.to_le_bytes());

    let err = DiskBackendState::decode(&bytes)
        .expect_err("should reject overlay block_size power_of_two");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("overlay block_size power_of_two")
    );
}

#[test]
fn disk_backend_state_rejects_invalid_overlay_present_byte() {
    // local backend, opfs kind, key="k", then invalid overlay_present=2.
    let mut bytes = Vec::new();
    bytes.push(0); // kind = local
    bytes.push(0); // backend_kind = opfs
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(b"k");
    bytes.push(2); // invalid overlay_present

    let err =
        DiskBackendState::decode(&bytes).expect_err("should reject invalid overlay_present byte");
    assert_eq!(err, SnapshotError::InvalidFieldEncoding("overlay_present"));
}

#[test]
fn disk_backend_state_rejects_excessive_overlay_block_size() {
    // Keep in sync with `MAX_OVERLAY_BLOCK_SIZE_BYTES` (64 MiB).
    const MAX_BLOCK: u32 = 64 * 1024 * 1024;

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
    // overlay.block_size_bytes = 128 MiB (invalid: too large, but still power-of-two and aligned)
    bytes.extend_from_slice(&(MAX_BLOCK * 2).to_le_bytes());

    let err =
        DiskBackendState::decode(&bytes).expect_err("should reject excessive overlay block size");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("overlay block_size too large")
    );
}

#[test]
fn disk_backend_state_rejects_invalid_validator_kind() {
    // remote backend with validator_kind=3 is invalid.
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

    // validator_kind = 3 (invalid)
    bytes.push(3);

    let err = DiskBackendState::decode(&bytes).expect_err("should reject invalid validator kind");
    assert_eq!(err, SnapshotError::InvalidFieldEncoding("validator_kind"));
}

#[test]
fn disk_backend_state_rejects_chunk_size_not_multiple_of_512() {
    // remote backend with chunk_size not aligned to 512 should fail before decoding overlay/cache.
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
    bytes.extend_from_slice(&513u32.to_le_bytes()); // chunk_size (invalid alignment)

    let err = DiskBackendState::decode(&bytes).expect_err("should reject unaligned chunk_size");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("chunk_size not multiple of 512")
    );
}

#[test]
fn disk_backend_state_rejects_excessive_chunk_size() {
    // Keep in sync with `MAX_REMOTE_CHUNK_SIZE_BYTES` (64 MiB).
    const MAX_CHUNK: u32 = 64 * 1024 * 1024;

    // remote backend with chunk_size too large should fail before decoding overlay/cache.
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
                   // Pick a value that remains 512-byte aligned but exceeds the maximum.
    bytes.extend_from_slice(&(MAX_CHUNK + SECTOR_SIZE as u32).to_le_bytes());

    let err = DiskBackendState::decode(&bytes).expect_err("should reject excessive chunk_size");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("chunk_size too large")
    );
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

    let mut state = DiskLayerState::new(backend, 4096, SECTOR_SIZE);
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
    w.field_u32(TAG_SECTOR_SIZE, SECTOR_SIZE as u32);
    w.field_u64(TAG_SIZE_BYTES, 513);

    let mut state = DiskLayerState::new(backend, 4096, SECTOR_SIZE);
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject disk sizes that aren't sector-aligned");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("disk_size not multiple of sector_size")
    );
}

#[test]
fn disk_layer_snapshot_rejects_excessive_legacy_backend_key_length() {
    const TAG_BACKEND_KEY: u16 = 1;
    const TAG_SECTOR_SIZE: u16 = 2;
    const TAG_SIZE_BYTES: u16 = 3;

    // Keep in sync with `MAX_DISK_STRING_BYTES` (64 KiB).
    const MAX_LEN: usize = 64 * 1024;

    let mut w = SnapshotWriter::new(DiskLayerState::DEVICE_ID, DiskLayerState::DEVICE_VERSION);
    w.field_bytes(TAG_BACKEND_KEY, vec![0u8; MAX_LEN + 1]);
    w.field_u32(TAG_SECTOR_SIZE, SECTOR_SIZE as u32);
    w.field_u64(TAG_SIZE_BYTES, 4096);

    let backend = DiskBackendState::Local(LocalDiskBackendState {
        kind: LocalDiskBackendKind::Other,
        key: "ignored".to_string(),
        overlay: None,
    });
    let mut state = DiskLayerState::new(backend, 4096, SECTOR_SIZE);
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject oversized legacy backend key");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("backend_key too long")
    );
}

#[test]
fn disk_layer_snapshot_rejects_legacy_backend_key_invalid_utf8() {
    const TAG_BACKEND_KEY: u16 = 1;
    const TAG_SECTOR_SIZE: u16 = 2;
    const TAG_SIZE_BYTES: u16 = 3;

    let mut w = SnapshotWriter::new(DiskLayerState::DEVICE_ID, DiskLayerState::DEVICE_VERSION);
    w.field_bytes(TAG_BACKEND_KEY, vec![0xff]);
    w.field_u32(TAG_SECTOR_SIZE, SECTOR_SIZE as u32);
    w.field_u64(TAG_SIZE_BYTES, 4096);

    let backend = DiskBackendState::Local(LocalDiskBackendState {
        kind: LocalDiskBackendKind::Other,
        key: "ignored".to_string(),
        overlay: None,
    });
    let mut state = DiskLayerState::new(backend, 4096, SECTOR_SIZE);
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject non-utf8 legacy backend key");
    assert_eq!(err, SnapshotError::InvalidFieldEncoding("backend_key utf8"));
}

#[test]
fn disk_layer_snapshot_rejects_zero_disk_size() {
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
    w.field_u32(TAG_SECTOR_SIZE, SECTOR_SIZE as u32);
    w.field_u64(TAG_SIZE_BYTES, 0);

    let mut state = DiskLayerState::new(backend, 4096, SECTOR_SIZE);
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject disk_size=0");
    assert_eq!(err, SnapshotError::InvalidFieldEncoding("disk_size"));
}

#[test]
fn disk_layer_snapshot_rejects_local_overlay_disk_size_mismatch() {
    const TAG_SECTOR_SIZE: u16 = 2;
    const TAG_SIZE_BYTES: u16 = 3;
    const TAG_BACKEND_STATE: u16 = 8;

    let backend = DiskBackendState::Local(LocalDiskBackendState {
        kind: LocalDiskBackendKind::Other,
        key: "disk0".to_string(),
        overlay: Some(aero_io_snapshot::io::storage::state::DiskOverlayState {
            file_name: "disk0.overlay".to_string(),
            disk_size_bytes: 4096,
            block_size_bytes: 1024 * 1024,
        }),
    });

    let mut w = SnapshotWriter::new(DiskLayerState::DEVICE_ID, DiskLayerState::DEVICE_VERSION);
    w.field_bytes(TAG_BACKEND_STATE, backend.encode());
    w.field_u32(TAG_SECTOR_SIZE, SECTOR_SIZE as u32);
    w.field_u64(TAG_SIZE_BYTES, 8192); // mismatch with overlay.disk_size_bytes

    let mut state = DiskLayerState::new(
        DiskBackendState::Local(LocalDiskBackendState {
            kind: LocalDiskBackendKind::Other,
            key: "ignored".to_string(),
            overlay: None,
        }),
        4096,
        SECTOR_SIZE,
    );
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject overlay disk_size mismatch");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("overlay disk_size mismatch")
    );
}

#[test]
fn disk_layer_snapshot_rejects_remote_overlay_disk_size_mismatch() {
    const TAG_SECTOR_SIZE: u16 = 2;
    const TAG_SIZE_BYTES: u16 = 3;
    const TAG_BACKEND_STATE: u16 = 8;

    let backend = DiskBackendState::Remote(
        aero_io_snapshot::io::storage::state::RemoteDiskBackendState {
            base: aero_io_snapshot::io::storage::state::RemoteDiskBaseState {
                image_id: "img".to_string(),
                version: "ver".to_string(),
                delivery_type: "range".to_string(),
                expected_validator: None,
                chunk_size: 1024 * 1024,
            },
            overlay: aero_io_snapshot::io::storage::state::DiskOverlayState {
                file_name: "remote.overlay".to_string(),
                disk_size_bytes: 4096,
                block_size_bytes: 1024 * 1024,
            },
            cache: aero_io_snapshot::io::storage::state::DiskCacheState {
                file_name: "remote.cache".to_string(),
            },
        },
    );

    let mut w = SnapshotWriter::new(DiskLayerState::DEVICE_ID, DiskLayerState::DEVICE_VERSION);
    w.field_bytes(TAG_BACKEND_STATE, backend.encode());
    w.field_u32(TAG_SECTOR_SIZE, SECTOR_SIZE as u32);
    w.field_u64(TAG_SIZE_BYTES, 8192); // mismatch with overlay.disk_size_bytes

    let mut state = DiskLayerState::new(
        DiskBackendState::Local(LocalDiskBackendState {
            kind: LocalDiskBackendKind::Other,
            key: "ignored".to_string(),
            overlay: None,
        }),
        4096,
        SECTOR_SIZE,
    );
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject remote overlay disk_size mismatch");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("overlay disk_size mismatch")
    );
}

#[test]
fn ahci_snapshot_rejects_excessive_port_count() {
    const TAG_PORTS: u16 = 2;

    let ports = Encoder::new().u32(u32::MAX).finish();

    let mut w = SnapshotWriter::new(
        AhciControllerState::DEVICE_ID,
        AhciControllerState::DEVICE_VERSION,
    );
    w.field_bytes(TAG_PORTS, ports);

    let mut state = AhciControllerState::default();
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject excessive AHCI port count");
    assert_eq!(err, SnapshotError::InvalidFieldEncoding("ahci port count"));
}

#[test]
fn ide_snapshot_rejects_oversized_pio_data_buffer() {
    let max_ide_pio = u32::try_from(MAX_IDE_DATA_BUFFER_BYTES).expect("max IDE buffer too large");

    const TAG_PRIMARY: u16 = 2;

    // Build a minimally-valid primary-channel payload that declares a huge PIO buffer length.
    // The decoder should reject it without attempting to allocate or read the bytes.
    let chan = Encoder::new()
        // ports
        .u16(0)
        .u16(0)
        .u8(0)
        // task file (6 regs + 5 HOB regs)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        // pending flags (5 bools)
        .bool(false)
        .bool(false)
        .bool(false)
        .bool(false)
        .bool(false)
        // status/error/control/irq
        .u8(0)
        .u8(0)
        .u8(0)
        .bool(false)
        // data_mode + transfer_kind
        .u8(0)
        .u8(0)
        // data_index + data_len
        .u32(0)
        .u32(max_ide_pio + 1)
        .finish();

    let mut w = SnapshotWriter::new(
        IdeControllerState::DEVICE_ID,
        IdeControllerState::DEVICE_VERSION,
    );
    w.field_bytes(TAG_PRIMARY, chan);

    let mut state = IdeControllerState::default();
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject oversized PIO buffer");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("ide pio buffer too large")
    );
}

#[test]
fn ide_snapshot_rejects_invalid_data_index() {
    const TAG_PRIMARY: u16 = 2;

    // Build a minimally-valid primary-channel payload that declares a data buffer length
    // smaller than the current data index.
    let chan = Encoder::new()
        // ports
        .u16(0)
        .u16(0)
        .u8(0)
        // task file (6 regs + 5 HOB regs)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        // pending flags (5 bools)
        .bool(false)
        .bool(false)
        .bool(false)
        .bool(false)
        .bool(false)
        // status/error/control/irq
        .u8(0)
        .u8(0)
        .u8(0)
        .bool(false)
        // data_mode + transfer_kind
        .u8(0)
        .u8(0)
        // data_index + data_len (data_index > data_len)
        .u32(2)
        .u32(1)
        .finish();

    let mut w = SnapshotWriter::new(
        IdeControllerState::DEVICE_ID,
        IdeControllerState::DEVICE_VERSION,
    );
    w.field_bytes(TAG_PRIMARY, chan);

    let mut state = IdeControllerState::default();
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject invalid data_index");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("ide pio data_index")
    );
}

#[test]
fn ide_snapshot_rejects_invalid_data_mode_enum() {
    const TAG_PRIMARY: u16 = 2;

    // `data_mode` is an enum encoded as a u8; only 0..=2 are valid.
    let chan = Encoder::new()
        // ports
        .u16(0)
        .u16(0)
        .u8(0)
        // task file (6 regs + 5 HOB regs)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        // pending flags (5 bools)
        .bool(false)
        .bool(false)
        .bool(false)
        .bool(false)
        .bool(false)
        // status/error/control/irq
        .u8(0)
        .u8(0)
        .u8(0)
        .bool(false)
        // invalid data_mode (=3)
        .u8(3)
        .finish();

    let mut w = SnapshotWriter::new(
        IdeControllerState::DEVICE_ID,
        IdeControllerState::DEVICE_VERSION,
    );
    w.field_bytes(TAG_PRIMARY, chan);

    let mut state = IdeControllerState::default();
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject invalid data_mode");
    assert_eq!(err, SnapshotError::InvalidFieldEncoding("ide data_mode"));
}

#[test]
fn ide_snapshot_rejects_invalid_transfer_kind_enum() {
    const TAG_PRIMARY: u16 = 2;

    // `transfer_kind` is an enum encoded as a u8; only 0..=5 are valid.
    let chan = Encoder::new()
        // ports
        .u16(0)
        .u16(0)
        .u8(0)
        // task file (6 regs + 5 HOB regs)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        // pending flags (5 bools)
        .bool(false)
        .bool(false)
        .bool(false)
        .bool(false)
        .bool(false)
        // status/error/control/irq
        .u8(0)
        .u8(0)
        .u8(0)
        .bool(false)
        // data_mode (valid) + invalid transfer_kind (=6)
        .u8(0)
        .u8(6)
        .finish();

    let mut w = SnapshotWriter::new(
        IdeControllerState::DEVICE_ID,
        IdeControllerState::DEVICE_VERSION,
    );
    w.field_bytes(TAG_PRIMARY, chan);

    let mut state = IdeControllerState::default();
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject invalid transfer_kind");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("ide transfer_kind")
    );
}

#[test]
fn ide_snapshot_rejects_invalid_drive_kind_enum() {
    const TAG_PRIMARY: u16 = 2;

    let chan = Encoder::new()
        // ports
        .u16(0)
        .u16(0)
        .u8(0)
        // task file (6 regs + 5 HOB regs)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        // pending flags (5 bools)
        .bool(false)
        .bool(false)
        .bool(false)
        .bool(false)
        .bool(false)
        // status/error/control/irq
        .u8(0)
        .u8(0)
        .u8(0)
        .bool(false)
        // data_mode + transfer_kind
        .u8(0)
        .u8(0)
        // data_index + data_len (empty buffer)
        .u32(0)
        .u32(0)
        // pio_write absent
        .u8(0)
        // pending_dma absent
        .u8(0)
        // bus master regs
        .u8(0)
        .u8(0)
        .u32(0)
        // invalid drive kind (=3)
        .u8(3)
        .finish();

    let mut w = SnapshotWriter::new(
        IdeControllerState::DEVICE_ID,
        IdeControllerState::DEVICE_VERSION,
    );
    w.field_bytes(TAG_PRIMARY, chan);

    let mut state = IdeControllerState::default();
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject invalid drive kind");
    assert_eq!(err, SnapshotError::InvalidFieldEncoding("ide drive kind"));
}

#[test]
fn ide_snapshot_rejects_invalid_pio_write_presence_byte() {
    const TAG_PRIMARY: u16 = 2;

    // The `pio_write` optional field is encoded as a presence byte (0 or 1).
    let chan = Encoder::new()
        // ports
        .u16(0)
        .u16(0)
        .u8(0)
        // task file (6 regs + 5 HOB regs)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        // pending flags (5 bools)
        .bool(false)
        .bool(false)
        .bool(false)
        .bool(false)
        .bool(false)
        // status/error/control/irq
        .u8(0)
        .u8(0)
        .u8(0)
        .bool(false)
        // data_mode + transfer_kind
        .u8(0)
        .u8(0)
        // data_index + data_len (empty buffer)
        .u32(0)
        .u32(0)
        // invalid pio_write present byte (=2)
        .u8(2)
        .finish();

    let mut w = SnapshotWriter::new(
        IdeControllerState::DEVICE_ID,
        IdeControllerState::DEVICE_VERSION,
    );
    w.field_bytes(TAG_PRIMARY, chan);

    let mut state = IdeControllerState::default();
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject invalid pio_write presence byte");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("ide pio_write present")
    );
}

#[test]
fn ide_snapshot_rejects_invalid_pending_dma_presence_byte() {
    const TAG_PRIMARY: u16 = 2;

    // The `pending_dma` optional field is encoded as a presence byte (0 or 1).
    let chan = Encoder::new()
        // ports
        .u16(0)
        .u16(0)
        .u8(0)
        // task file (6 regs + 5 HOB regs)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        // pending flags (5 bools)
        .bool(false)
        .bool(false)
        .bool(false)
        .bool(false)
        .bool(false)
        // status/error/control/irq
        .u8(0)
        .u8(0)
        .u8(0)
        .bool(false)
        // data_mode + transfer_kind
        .u8(0)
        .u8(0)
        // data_index + data_len (empty buffer)
        .u32(0)
        .u32(0)
        // pio_write absent
        .u8(0)
        // invalid pending_dma present byte (=2)
        .u8(2)
        .finish();

    let mut w = SnapshotWriter::new(
        IdeControllerState::DEVICE_ID,
        IdeControllerState::DEVICE_VERSION,
    );
    w.field_bytes(TAG_PRIMARY, chan);

    let mut state = IdeControllerState::default();
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject invalid pending_dma presence byte");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("ide pending_dma present")
    );
}

#[test]
fn ide_snapshot_rejects_invalid_dma_direction_enum() {
    const TAG_PRIMARY: u16 = 2;

    // DMA direction is encoded as a u8; only 0..=1 are valid.
    let chan = Encoder::new()
        // ports
        .u16(0)
        .u16(0)
        .u8(0)
        // task file (6 regs + 5 HOB regs)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        // pending flags (5 bools)
        .bool(false)
        .bool(false)
        .bool(false)
        .bool(false)
        .bool(false)
        // status/error/control/irq
        .u8(0)
        .u8(0)
        .u8(0)
        .bool(false)
        // data_mode + transfer_kind
        .u8(0)
        .u8(0)
        // data_index + data_len (empty buffer)
        .u32(0)
        .u32(0)
        // pio_write absent
        .u8(0)
        // pending_dma present
        .u8(1)
        // invalid dma direction (=2)
        .u8(2)
        .finish();

    let mut w = SnapshotWriter::new(
        IdeControllerState::DEVICE_ID,
        IdeControllerState::DEVICE_VERSION,
    );
    w.field_bytes(TAG_PRIMARY, chan);

    let mut state = IdeControllerState::default();
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject invalid dma direction");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("ide dma direction")
    );
}

#[test]
fn ide_snapshot_rejects_invalid_dma_commit_kind() {
    const TAG_PRIMARY: u16 = 2;

    // DMA commit kind is encoded as a u8; only 0..=1 are valid.
    let chan = Encoder::new()
        // ports
        .u16(0)
        .u16(0)
        .u8(0)
        // task file (6 regs + 5 HOB regs)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        // pending flags (5 bools)
        .bool(false)
        .bool(false)
        .bool(false)
        .bool(false)
        .bool(false)
        // status/error/control/irq
        .u8(0)
        .u8(0)
        .u8(0)
        .bool(false)
        // data_mode + transfer_kind
        .u8(0)
        .u8(0)
        // data_index + data_len (empty buffer)
        .u32(0)
        .u32(0)
        // pio_write absent
        .u8(0)
        // pending_dma present
        .u8(1)
        // dma direction (valid)
        .u8(0)
        // dma buffer length (0)
        .u32(0)
        // invalid commit kind (=2)
        .u8(2)
        .finish();

    let mut w = SnapshotWriter::new(
        IdeControllerState::DEVICE_ID,
        IdeControllerState::DEVICE_VERSION,
    );
    w.field_bytes(TAG_PRIMARY, chan);

    let mut state = IdeControllerState::default();
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject invalid dma commit kind");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("ide dma commit kind")
    );
}

#[test]
fn ide_snapshot_rejects_oversized_dma_buffer() {
    let max_ide_buf = u32::try_from(MAX_IDE_DATA_BUFFER_BYTES).expect("max IDE buffer too large");

    const TAG_PRIMARY: u16 = 2;

    // Build a minimally-valid primary-channel payload with a pending DMA request that
    // declares an excessive buffer length. The decoder should reject it without allocating.
    let chan = Encoder::new()
        // ports
        .u16(0)
        .u16(0)
        .u8(0)
        // task file (6 regs + 5 HOB regs)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        // pending flags (5 bools)
        .bool(false)
        .bool(false)
        .bool(false)
        .bool(false)
        .bool(false)
        // status/error/control/irq
        .u8(0)
        .u8(0)
        .u8(0)
        .bool(false)
        // data_mode + transfer_kind
        .u8(0)
        .u8(0)
        // data_index + data_len + data bytes (empty)
        .u32(0)
        .u32(0)
        // pio_write absent
        .u8(0)
        // pending_dma present
        .u8(1)
        // dma direction
        .u8(0)
        // dma buffer length (oversized)
        .u32(max_ide_buf + 1)
        .finish();

    let mut w = SnapshotWriter::new(
        IdeControllerState::DEVICE_ID,
        IdeControllerState::DEVICE_VERSION,
    );
    w.field_bytes(TAG_PRIMARY, chan);

    let mut state = IdeControllerState::default();
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject oversized DMA buffer");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("ide dma buffer too large")
    );
}

#[test]
fn ide_snapshot_rejects_short_atapi_packet_buffer() {
    const TAG_PRIMARY: u16 = 2;

    // Build a minimally-valid primary-channel payload that claims an ATAPI PACKET transfer
    // but provides a data buffer smaller than 12 bytes. The emulator assumes a 12-byte packet
    // and would panic if this were allowed through decoding.
    let chan = Encoder::new()
        // ports
        .u16(0)
        .u16(0)
        .u8(0)
        // task file (6 regs + 5 HOB regs)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        .u8(0)
        // pending flags (5 bools)
        .bool(false)
        .bool(false)
        .bool(false)
        .bool(false)
        .bool(false)
        // status/error/control/irq
        .u8(0)
        .u8(0)
        .u8(0)
        .bool(false)
        // data_mode + transfer_kind (AtapiPacket=4)
        .u8(2) // PioOut
        .u8(4)
        // data_index + data_len (len too small)
        .u32(0)
        .u32(1)
        .bytes(&[0u8])
        // pio_write absent
        .u8(0)
        // pending_dma absent
        .u8(0)
        // bus master regs
        .u8(0)
        .u8(0)
        .u32(0)
        // drives (2)
        .u8(0)
        .u8(0)
        .finish();

    let mut w = SnapshotWriter::new(
        IdeControllerState::DEVICE_ID,
        IdeControllerState::DEVICE_VERSION,
    );
    w.field_bytes(TAG_PRIMARY, chan);

    let mut state = IdeControllerState::default();
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject short ATAPI packet buffer");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("ide atapi packet buffer too small")
    );
}
