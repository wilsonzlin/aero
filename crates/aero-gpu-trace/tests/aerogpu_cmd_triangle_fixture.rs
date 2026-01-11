use aero_gpu_trace::{AerogpuMemoryRangeCapture, TraceMeta, TraceReader, TraceWriter};
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode, AerogpuCmdStreamHeader as ProtocolCmdStreamHeader,
    AerogpuPrimitiveTopology, AEROGPU_CLEAR_COLOR, AEROGPU_CMD_STREAM_MAGIC,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32};
use aero_protocol::aerogpu::aerogpu_ring::{
    AerogpuAllocEntry as ProtocolAllocEntry, AerogpuAllocTableHeader as ProtocolAllocTableHeader,
    AEROGPU_ALLOC_TABLE_MAGIC, AEROGPU_SUBMIT_FLAG_PRESENT,
};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = AerogpuFormat::R8G8B8A8Unorm as u32;
const AEROGPU_TOPOLOGY_TRIANGLELIST: u32 = AerogpuPrimitiveTopology::TriangleList as u32;

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(ProtocolCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(ProtocolCmdHdr, size_bytes);
const ALLOC_TABLE_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(ProtocolAllocTableHeader, size_bytes);

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/aerogpu_cmd_triangle.aerogputrace")
}

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_f32_bits(out: &mut Vec<u8>, v: f32) {
    push_u32(out, v.to_bits());
}

fn emit_packet(bytes: &mut Vec<u8>, opcode: u32, payload: impl FnOnce(&mut Vec<u8>)) {
    let start = bytes.len();
    push_u32(bytes, opcode);
    push_u32(bytes, 0); // size_bytes placeholder
    payload(bytes);

    let size_bytes = (bytes.len() - start) as u32;
    assert!(size_bytes >= 8);
    assert_eq!(size_bytes % 4, 0);
    bytes[start + CMD_HDR_SIZE_BYTES_OFFSET..start + CMD_HDR_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size_bytes.to_le_bytes());
}

fn build_cmd_stream(packets: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut bytes = Vec::new();
    // aerogpu_cmd_stream_header
    push_u32(&mut bytes, AEROGPU_CMD_STREAM_MAGIC);
    push_u32(&mut bytes, AEROGPU_ABI_VERSION_U32);
    push_u32(&mut bytes, 0); // size_bytes (patched later)
    push_u32(&mut bytes, 0); // flags
    push_u32(&mut bytes, 0); // reserved0
    push_u32(&mut bytes, 0); // reserved1
    assert_eq!(bytes.len(), ProtocolCmdStreamHeader::SIZE_BYTES);

    packets(&mut bytes);

    let size_bytes = bytes.len() as u32;
    bytes[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size_bytes.to_le_bytes());
    bytes
}

fn build_alloc_table(vertex_gpa: u64, vertex_bytes: &[u8]) -> Vec<u8> {
    let entry_stride = ProtocolAllocEntry::SIZE_BYTES as u32;
    let header_size = ProtocolAllocTableHeader::SIZE_BYTES as u32;

    let mut bytes = Vec::new();
    push_u32(&mut bytes, AEROGPU_ALLOC_TABLE_MAGIC);
    push_u32(&mut bytes, AEROGPU_ABI_VERSION_U32);
    push_u32(&mut bytes, 0); // size_bytes (patched later)
    push_u32(&mut bytes, 1); // entry_count
    push_u32(&mut bytes, entry_stride);
    push_u32(&mut bytes, 0); // reserved0
    assert_eq!(bytes.len(), header_size as usize);

    // aerogpu_alloc_entry
    push_u32(&mut bytes, 1); // alloc_id
    push_u32(&mut bytes, 0); // flags
    push_u64(&mut bytes, vertex_gpa);
    push_u64(&mut bytes, vertex_bytes.len() as u64);
    push_u64(&mut bytes, 0); // reserved0

    let size_bytes = bytes.len() as u32;
    bytes[ALLOC_TABLE_SIZE_BYTES_OFFSET..ALLOC_TABLE_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size_bytes.to_le_bytes());
    assert_eq!(bytes.len(), size_bytes as usize);
    bytes
}

fn generate_trace() -> Vec<u8> {
    let vertex_gpa = 0x1000u64;

    // Fullscreen triangle, solid red.
    let vertices: [f32; 18] = [
        -1.0, -1.0, 1.0, 0.0, 0.0, 1.0, // v0
        3.0, -1.0, 1.0, 0.0, 0.0, 1.0, // v1
        -1.0, 3.0, 1.0, 0.0, 0.0, 1.0, // v2
    ];
    let mut vertex_bytes = Vec::with_capacity(vertices.len() * 4);
    for f in vertices {
        vertex_bytes.extend_from_slice(&f.to_le_bytes());
    }

    let alloc_table = build_alloc_table(vertex_gpa, &vertex_bytes);

    let cmd_stream = build_cmd_stream(|out| {
        // CREATE_BUFFER (handle=1), backed by alloc_id 1.
        emit_packet(out, AerogpuCmdOpcode::CreateBuffer as u32, |p| {
            push_u32(p, 1); // buffer_handle
            push_u32(p, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER);
            push_u64(p, vertex_bytes.len() as u64);
            push_u32(p, 1); // backing_alloc_id
            push_u32(p, 0); // backing_offset_bytes
            push_u64(p, 0); // reserved0
        });

        // CREATE_TEXTURE2D (handle=2) as render target.
        emit_packet(out, AerogpuCmdOpcode::CreateTexture2d as u32, |p| {
            push_u32(p, 2); // texture_handle
            push_u32(p, AEROGPU_RESOURCE_USAGE_RENDER_TARGET);
            push_u32(p, AEROGPU_FORMAT_R8G8B8A8_UNORM);
            push_u32(p, 64); // width
            push_u32(p, 64); // height
            push_u32(p, 1); // mip_levels
            push_u32(p, 1); // array_layers
            push_u32(p, 0); // row_pitch_bytes
            push_u32(p, 0); // backing_alloc_id
            push_u32(p, 0); // backing_offset_bytes
            push_u64(p, 0); // reserved0
        });

        // SET_RENDER_TARGETS: color0 = texture 2.
        emit_packet(out, AerogpuCmdOpcode::SetRenderTargets as u32, |p| {
            push_u32(p, 1); // color_count
            push_u32(p, 0); // depth_stencil
            push_u32(p, 2); // colors[0]
            for _ in 1..8 {
                push_u32(p, 0);
            }
        });

        // SET_VIEWPORT: full target.
        emit_packet(out, AerogpuCmdOpcode::SetViewport as u32, |p| {
            push_f32_bits(p, 0.0);
            push_f32_bits(p, 0.0);
            push_f32_bits(p, 64.0);
            push_f32_bits(p, 64.0);
            push_f32_bits(p, 0.0);
            push_f32_bits(p, 1.0);
        });

        // CLEAR: opaque black.
        emit_packet(out, AerogpuCmdOpcode::Clear as u32, |p| {
            push_u32(p, AEROGPU_CLEAR_COLOR);
            push_f32_bits(p, 0.0);
            push_f32_bits(p, 0.0);
            push_f32_bits(p, 0.0);
            push_f32_bits(p, 1.0);
            push_f32_bits(p, 1.0); // depth
            push_u32(p, 0); // stencil
        });

        // SET_VERTEX_BUFFERS: slot0 = buffer 1.
        emit_packet(out, AerogpuCmdOpcode::SetVertexBuffers as u32, |p| {
            push_u32(p, 0); // start_slot
            push_u32(p, 1); // buffer_count
                            // aerogpu_vertex_buffer_binding
            push_u32(p, 1); // buffer
            push_u32(p, 24); // stride_bytes
            push_u32(p, 0); // offset_bytes
            push_u32(p, 0); // reserved0
        });

        emit_packet(out, AerogpuCmdOpcode::SetPrimitiveTopology as u32, |p| {
            push_u32(p, AEROGPU_TOPOLOGY_TRIANGLELIST);
            push_u32(p, 0);
        });

        // DRAW: 3 vertices, 1 instance.
        emit_packet(out, AerogpuCmdOpcode::Draw as u32, |p| {
            push_u32(p, 3); // vertex_count
            push_u32(p, 1); // instance_count
            push_u32(p, 0); // first_vertex
            push_u32(p, 0); // first_instance
        });

        // PRESENT.
        emit_packet(out, AerogpuCmdOpcode::Present as u32, |p| {
            push_u32(p, 0); // scanout_id
            push_u32(p, 0); // flags
        });
    });

    let meta = TraceMeta::new("0.0.0-dev", AEROGPU_ABI_VERSION_U32);
    let mut writer = TraceWriter::new_v2(Vec::<u8>::new(), &meta).expect("TraceWriter::new_v2");

    writer.begin_frame(0).unwrap();
    writer
        .write_aerogpu_submission(
            AEROGPU_SUBMIT_FLAG_PRESENT,
            0, // context_id
            0, // engine_id
            1, // signal_fence
            &cmd_stream,
            Some(&alloc_table),
            &[AerogpuMemoryRangeCapture {
                alloc_id: 1,
                flags: 0,
                gpa: vertex_gpa,
                size_bytes: vertex_bytes.len() as u64,
                bytes: &vertex_bytes,
            }],
        )
        .unwrap();
    writer.present(0).unwrap();
    let bytes = writer.finish().unwrap();

    // Sanity-check: it must parse, and contain exactly one frame.
    let reader = TraceReader::open(Cursor::new(bytes.clone())).expect("TraceReader::open");
    assert_eq!(reader.header.command_abi_version, AEROGPU_ABI_VERSION_U32);
    assert_eq!(reader.frame_entries().len(), 1);

    bytes
}

#[test]
fn aerogpu_cmd_triangle_trace_fixture_is_stable() {
    let bytes = generate_trace();
    let path = fixture_path();

    if std::env::var_os("AERO_UPDATE_TRACE_FIXTURES").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &bytes).unwrap();
        return;
    }

    let fixture =
        fs::read(&path).expect("fixture file missing; run with AERO_UPDATE_TRACE_FIXTURES=1");
    assert_eq!(bytes, fixture);
}
