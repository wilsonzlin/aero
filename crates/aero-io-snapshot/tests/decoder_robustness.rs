use aero_io_snapshot::io::state::codec::Decoder;
use aero_io_snapshot::io::state::{SnapshotError, SnapshotReader};

#[test]
fn decoder_vec_bytes_does_not_preallocate_on_large_count() {
    // `Decoder::vec_bytes` reads a u32 element count, followed by `count` (len + bytes) entries.
    // Historically it used `Vec::with_capacity(count)`, which could attempt to allocate a
    // pathological amount of memory for corrupted/truncated snapshots. This test ensures we return
    // a normal decode error without trying to preallocate.
    let buf = u32::MAX.to_le_bytes();
    let mut d = Decoder::new(&buf);
    let err = d.vec_bytes().unwrap_err();
    assert_eq!(err, SnapshotError::UnexpectedEof);
}

#[test]
fn decoder_take_rejects_len_overflow_without_panic() {
    // `Decoder::take` previously used `self.offset + len` without checked arithmetic, which can
    // overflow on 32-bit targets (including wasm32) and panic when slicing. Ensure we return a
    // normal decode error instead.
    let buf = [0u8; 2];
    let mut d = Decoder::new(&buf);
    let _ = d.u8().unwrap(); // advance offset to 1

    let err = d.bytes(usize::MAX).unwrap_err();
    assert_eq!(err, SnapshotError::UnexpectedEof);
}

#[test]
fn snapshot_reader_rejects_excessive_field_count() {
    // SnapshotReader stores fields in a BTreeMap keyed by tag. A corrupted snapshot can encode many
    // tiny fields (tag + len + empty) and force pathological allocations. Ensure we cap the field
    // count.
    const DEVICE_ID: [u8; 4] = *b"TEST";

    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"AERO");
    bytes.extend_from_slice(&1u16.to_le_bytes()); // format version major
    bytes.extend_from_slice(&0u16.to_le_bytes()); // format version minor
    bytes.extend_from_slice(&DEVICE_ID);
    bytes.extend_from_slice(&1u16.to_le_bytes()); // device version major
    bytes.extend_from_slice(&0u16.to_le_bytes()); // device version minor

    // MAX_FIELDS is 4096 (see SnapshotReader::parse). Emit 4097 unique zero-length fields.
    for tag in 0u16..=4096u16 {
        bytes.extend_from_slice(&tag.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
    }

    let err = match SnapshotReader::parse(&bytes, DEVICE_ID) {
        Ok(_) => panic!("expected SnapshotReader::parse to reject excessive field count"),
        Err(err) => err,
    };
    assert_eq!(err, SnapshotError::InvalidFieldEncoding("too many fields"));
}

#[test]
fn snapshot_reader_rejects_duplicate_field_tags() {
    const DEVICE_ID: [u8; 4] = *b"TEST";

    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"AERO");
    bytes.extend_from_slice(&1u16.to_le_bytes()); // format version major
    bytes.extend_from_slice(&0u16.to_le_bytes()); // format version minor
    bytes.extend_from_slice(&DEVICE_ID);
    bytes.extend_from_slice(&1u16.to_le_bytes()); // device version major
    bytes.extend_from_slice(&0u16.to_le_bytes()); // device version minor

    // Two identical tags (1) with empty payloads.
    for _ in 0..2 {
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
    }

    let err = match SnapshotReader::parse(&bytes, DEVICE_ID) {
        Ok(_) => panic!("expected SnapshotReader::parse to reject duplicate field tags"),
        Err(err) => err,
    };
    assert_eq!(err, SnapshotError::DuplicateFieldTag(1));
}

#[test]
fn snapshot_reader_rejects_invalid_magic() {
    const DEVICE_ID: [u8; 4] = *b"TEST";

    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"NOPE");
    bytes.extend_from_slice(&1u16.to_le_bytes()); // format version major
    bytes.extend_from_slice(&0u16.to_le_bytes()); // format version minor
    bytes.extend_from_slice(&DEVICE_ID);
    bytes.extend_from_slice(&1u16.to_le_bytes()); // device version major
    bytes.extend_from_slice(&0u16.to_le_bytes()); // device version minor

    let err = match SnapshotReader::parse(&bytes, DEVICE_ID) {
        Ok(_) => panic!("expected SnapshotReader::parse to reject invalid magic"),
        Err(err) => err,
    };
    assert_eq!(err, SnapshotError::InvalidMagic);
}

#[test]
fn snapshot_reader_rejects_device_id_mismatch() {
    const EXPECTED: [u8; 4] = *b"TEST";

    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"AERO");
    bytes.extend_from_slice(&1u16.to_le_bytes()); // format version major
    bytes.extend_from_slice(&0u16.to_le_bytes()); // format version minor
    bytes.extend_from_slice(b"NOPE"); // wrong device id
    bytes.extend_from_slice(&1u16.to_le_bytes()); // device version major
    bytes.extend_from_slice(&0u16.to_le_bytes()); // device version minor

    let err = match SnapshotReader::parse(&bytes, EXPECTED) {
        Ok(_) => panic!("expected SnapshotReader::parse to reject device id mismatch"),
        Err(err) => err,
    };
    assert_eq!(
        err,
        SnapshotError::DeviceIdMismatch {
            expected: EXPECTED,
            found: *b"NOPE",
        }
    );
}

#[test]
fn snapshot_reader_rejects_unsupported_format_major() {
    const DEVICE_ID: [u8; 4] = *b"TEST";

    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"AERO");
    bytes.extend_from_slice(&2u16.to_le_bytes()); // format version major (unsupported)
    bytes.extend_from_slice(&0u16.to_le_bytes()); // format version minor
    bytes.extend_from_slice(&DEVICE_ID);
    bytes.extend_from_slice(&1u16.to_le_bytes()); // device version major
    bytes.extend_from_slice(&0u16.to_le_bytes()); // device version minor

    let err = match SnapshotReader::parse(&bytes, DEVICE_ID) {
        Ok(_) => panic!("expected SnapshotReader::parse to reject unsupported format major"),
        Err(err) => err,
    };
    assert!(matches!(
        err,
        SnapshotError::UnsupportedFormatVersion { .. }
    ));
}
