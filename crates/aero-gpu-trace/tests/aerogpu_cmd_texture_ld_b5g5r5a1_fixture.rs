use aero_gpu_trace::{AerogpuSubmissionCapture, TraceMeta, TraceReader, TraceWriter};
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuPrimitiveTopology, AerogpuShaderStage, AerogpuVertexBufferBinding, AEROGPU_CLEAR_COLOR,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_TEXTURE,
    AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32};
use aero_protocol::aerogpu::aerogpu_ring::AEROGPU_SUBMIT_FLAG_PRESENT;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

const DXBC_VS_PASSTHROUGH: &[u8] =
    include_bytes!("../../aero-d3d11/tests/fixtures/vs_passthrough.dxbc");
const DXBC_PS_LD: &[u8] = include_bytes!("../../aero-d3d11/tests/fixtures/ps_ld.dxbc");
const ILAY_POS3_COLOR: &[u8] =
    include_bytes!("../../aero-d3d11/tests/fixtures/ilay_pos3_color.bin");

fn fixture_path() -> PathBuf {
    // `CARGO_MANIFEST_DIR` = `.../crates/aero-gpu-trace`
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/aerogpu_cmd_texture_ld_b5g5r5a1.aerogputrace")
}

fn make_cmd_stream() -> Vec<u8> {
    // Resource handles are arbitrary stable integers.
    const VB: u32 = 1;
    const TEX: u32 = 2;
    const RT: u32 = 3;
    const VS: u32 = 10;
    const PS: u32 = 11;
    const IL: u32 = 20;

    // Fullscreen triangle; vertex color is unused (pixel shader uses `ld` from a texture).
    //
    // Vertex format: float3 position + float4 color.
    let vertices: [f32; 21] = [
        -1.0, -1.0, 0.0, 0.0, 0.0, 0.0, 1.0, // v0
        -1.0, 3.0, 0.0, 0.0, 0.0, 0.0, 1.0, // v1
        3.0, -1.0, 0.0, 0.0, 0.0, 0.0, 1.0, // v2
    ];
    let mut vb_bytes = Vec::with_capacity(vertices.len() * 4);
    for f in vertices {
        vb_bytes.extend_from_slice(&f.to_le_bytes());
    }

    // 1x1 B5G5R5A1 texture with value 0x7FFF (white RGB, alpha=0).
    //
    // Executor expands this to RGBA8 using bit replication + 1-bit alpha:
    // r5=g5=b5=31, a1=0 -> rgba8 = [255,255,255,0].
    let texel_b5g5r5a1: [u8; 2] = 0x7FFFu16.to_le_bytes();

    let mut w = AerogpuCmdWriter::new();

    w.create_buffer(
        VB,
        AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
        vb_bytes.len() as u64,
        0,
        0,
    );
    w.upload_resource(VB, 0, &vb_bytes);

    w.create_texture2d(
        TEX,
        AEROGPU_RESOURCE_USAGE_TEXTURE,
        AerogpuFormat::B5G5R5A1Unorm as u32,
        1,
        1,
        1,
        1,
        0,
        0,
        0,
    );
    w.upload_resource(TEX, 0, &texel_b5g5r5a1);

    // Render target is RGBA8 so readback is deterministic.
    w.create_texture2d(
        RT,
        AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
        AerogpuFormat::R8G8B8A8Unorm as u32,
        64,
        64,
        1,
        1,
        0,
        0,
        0,
    );

    w.set_render_targets(&[RT], 0);
    w.set_viewport(0.0, 0.0, 64.0, 64.0, 0.0, 1.0);
    w.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 0.0, 1.0], 1.0, 0);

    w.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, DXBC_VS_PASSTHROUGH);
    w.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, DXBC_PS_LD);
    w.bind_shaders(VS, PS, 0);

    w.create_input_layout(IL, ILAY_POS3_COLOR);
    w.set_input_layout(IL);

    let binding = AerogpuVertexBufferBinding {
        buffer: VB,
        stride_bytes: 28, // float3 + float4
        offset_bytes: 0,
        reserved0: 0,
    };
    w.set_vertex_buffers(0, &[binding]);

    w.set_primitive_topology(AerogpuPrimitiveTopology::TriangleList);
    w.set_texture(AerogpuShaderStage::Pixel, 0, TEX);
    w.draw(3, 1, 0, 0);
    w.present(0, 0);

    w.finish()
}

fn generate_trace() -> Vec<u8> {
    let cmd_stream = make_cmd_stream();

    let meta = TraceMeta::new("0.0.0-dev", AEROGPU_ABI_VERSION_U32);
    let mut writer = TraceWriter::new_v2(Vec::<u8>::new(), &meta).expect("TraceWriter::new_v2");

    writer.begin_frame(0).unwrap();
    writer
        .write_aerogpu_submission(AerogpuSubmissionCapture {
            submit_flags: AEROGPU_SUBMIT_FLAG_PRESENT,
            context_id: 0,
            engine_id: 0,
            signal_fence: 1,
            cmd_stream_bytes: &cmd_stream,
            alloc_table_bytes: None,
            memory_ranges: &[],
        })
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
fn aerogpu_cmd_texture_ld_b5g5r5a1_trace_fixture_is_stable() {
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

