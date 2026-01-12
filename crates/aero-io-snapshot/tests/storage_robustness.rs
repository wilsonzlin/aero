use aero_io_snapshot::io::state::codec::Encoder;
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotError, SnapshotWriter};
use aero_io_snapshot::io::storage::state::{
    AhciControllerState, DiskBackendState, DiskLayerState, IdeControllerState, LocalDiskBackendKind,
    LocalDiskBackendState, NvmeControllerState, MAX_IDE_DATA_BUFFER_BYTES,
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
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("ahci port count")
    );
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
    assert_eq!(err, SnapshotError::InvalidFieldEncoding("ide pio data_index"));
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
    assert_eq!(err, SnapshotError::InvalidFieldEncoding("ide transfer_kind"));
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
