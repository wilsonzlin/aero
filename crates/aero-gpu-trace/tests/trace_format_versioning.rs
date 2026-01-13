use aero_gpu_trace::{TraceMeta, TraceReadError, TraceReader, TraceWriter, CONTAINER_VERSION};
use std::io::Cursor;

fn minimal_trace_bytes(command_abi_version: u32) -> Vec<u8> {
    let meta = TraceMeta::new("test", command_abi_version);
    let mut writer = TraceWriter::new(Vec::<u8>::new(), &meta).expect("TraceWriter::new");
    writer.begin_frame(0).unwrap();
    writer.present(0).unwrap();
    writer.finish().unwrap()
}

#[test]
fn reject_trace_with_wrong_magic() {
    let mut bytes = minimal_trace_bytes(0);

    // Corrupt the first byte of the 8-byte trace header magic.
    bytes[0] ^= 0xFF;

    let err = match TraceReader::open(Cursor::new(bytes)) {
        Ok(_) => panic!("expected trace open to fail"),
        Err(err) => err,
    };
    assert!(matches!(err, TraceReadError::InvalidMagic));
}

#[test]
fn reject_trace_with_unknown_newer_container_version() {
    let mut bytes = minimal_trace_bytes(0);

    // TraceHeader layout (little-endian):
    // [0..8]   magic
    // [8..12]  header_size
    // [12..16] container_version
    let bad_version = CONTAINER_VERSION + 1;
    bytes[12..16].copy_from_slice(&bad_version.to_le_bytes());

    let err = match TraceReader::open(Cursor::new(bytes)) {
        Ok(_) => panic!("expected trace open to fail"),
        Err(err) => err,
    };
    assert!(matches!(
        err,
        TraceReadError::UnsupportedContainerVersion(v) if v == bad_version
    ));
}

#[test]
fn accept_trace_with_older_aerogpu_abi_minor_version() {
    use aero_protocol::aerogpu::aerogpu_pci::{
        abi_major, abi_minor, AEROGPU_ABI_MAJOR, AEROGPU_ABI_VERSION_U32,
    };

    // The AeroGPU protocol defines ABI minor versions as backwards-compatible extensions.
    // A trace recorded against an older minor version should still be readable by the trace
    // container reader (the container stores the value but does not interpret packet bytes).
    let older_minor_version: u32 = (AEROGPU_ABI_MAJOR << 16) | 0;

    // Assert this really is an "older or equal minor" version of the current ABI major.
    assert_eq!(
        abi_major(older_minor_version),
        abi_major(AEROGPU_ABI_VERSION_U32)
    );
    assert!(abi_minor(older_minor_version) <= abi_minor(AEROGPU_ABI_VERSION_U32));

    let bytes = minimal_trace_bytes(older_minor_version);
    let reader = TraceReader::open(Cursor::new(bytes)).expect("TraceReader::open");
    assert_eq!(reader.header.command_abi_version, older_minor_version);
}
