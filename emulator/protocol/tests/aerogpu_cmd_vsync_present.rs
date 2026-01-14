use aero_protocol::aerogpu::aerogpu_cmd::{
    cmd_stream_has_vsync_present_bytes, cmd_stream_has_vsync_present_reader, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader, AEROGPU_CMD_STREAM_MAGIC, AEROGPU_PRESENT_FLAG_VSYNC,
};
use aero_protocol::aerogpu::aerogpu_pci::AEROGPU_ABI_VERSION_U32;

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(AerogpuCmdStreamHeader, size_bytes);

fn push_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn build_cmd_stream_header() -> Vec<u8> {
    let mut bytes = Vec::new();
    push_u32(&mut bytes, AEROGPU_CMD_STREAM_MAGIC);
    push_u32(&mut bytes, AEROGPU_ABI_VERSION_U32);
    push_u32(&mut bytes, 0); // size_bytes (patched later)
    push_u32(&mut bytes, 0); // flags
    push_u32(&mut bytes, 0); // reserved0
    push_u32(&mut bytes, 0); // reserved1
    bytes
}

fn patch_stream_size(bytes: &mut [u8]) {
    let size_bytes = u32::try_from(bytes.len()).unwrap();
    bytes[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size_bytes.to_le_bytes());
}

#[test]
fn detects_vsync_present_packets() {
    // PRESENT with VSYNC.
    let mut stream = build_cmd_stream_header();
    push_u32(&mut stream, AerogpuCmdOpcode::Present as u32);
    push_u32(&mut stream, 16); // size_bytes
    push_u32(&mut stream, 0); // scanout_id
    push_u32(&mut stream, AEROGPU_PRESENT_FLAG_VSYNC);
    patch_stream_size(&mut stream);

    assert!(cmd_stream_has_vsync_present_bytes(&stream).unwrap());

    let base_gpa = 0x1000u64;
    let stream_copy = stream.clone();
    let read = |gpa: u64, buf: &mut [u8]| {
        let off = usize::try_from(gpa - base_gpa).unwrap();
        let end = off + buf.len();
        buf.copy_from_slice(&stream_copy[off..end]);
    };
    assert!(cmd_stream_has_vsync_present_reader(
        read,
        base_gpa,
        stream.len() as u32
    )
    .unwrap());

    // PRESENT without VSYNC.
    let mut stream = build_cmd_stream_header();
    push_u32(&mut stream, AerogpuCmdOpcode::Present as u32);
    push_u32(&mut stream, 16); // size_bytes
    push_u32(&mut stream, 0); // scanout_id
    push_u32(&mut stream, 0); // flags
    patch_stream_size(&mut stream);

    assert!(!cmd_stream_has_vsync_present_bytes(&stream).unwrap());

    let base_gpa = 0x2000u64;
    let stream_copy = stream.clone();
    let read = |gpa: u64, buf: &mut [u8]| {
        let off = usize::try_from(gpa - base_gpa).unwrap();
        let end = off + buf.len();
        buf.copy_from_slice(&stream_copy[off..end]);
    };
    assert!(!cmd_stream_has_vsync_present_reader(
        read,
        base_gpa,
        stream.len() as u32
    )
    .unwrap());

    // PRESENT_EX with VSYNC.
    let mut stream = build_cmd_stream_header();
    push_u32(&mut stream, AerogpuCmdOpcode::PresentEx as u32);
    push_u32(&mut stream, 24); // size_bytes
    push_u32(&mut stream, 0); // scanout_id
    push_u32(&mut stream, AEROGPU_PRESENT_FLAG_VSYNC);
    push_u32(&mut stream, 0); // d3d9_present_flags
    push_u32(&mut stream, 0); // reserved0
    patch_stream_size(&mut stream);

    assert!(cmd_stream_has_vsync_present_bytes(&stream).unwrap());

    let base_gpa = 0x3000u64;
    let stream_copy = stream.clone();
    let read = |gpa: u64, buf: &mut [u8]| {
        let off = usize::try_from(gpa - base_gpa).unwrap();
        let end = off + buf.len();
        buf.copy_from_slice(&stream_copy[off..end]);
    };
    assert!(cmd_stream_has_vsync_present_reader(read, base_gpa, stream.len() as u32).unwrap(),);
}
