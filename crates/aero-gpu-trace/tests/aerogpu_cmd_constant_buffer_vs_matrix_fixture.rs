use aero_gpu_trace::{AerogpuSubmissionCapture, TraceMeta, TraceReader, TraceWriter};
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuConstantBufferBinding, AerogpuPrimitiveTopology, AerogpuShaderStage,
    AerogpuVertexBufferBinding, AEROGPU_CLEAR_COLOR, AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32};
use aero_protocol::aerogpu::aerogpu_ring::AEROGPU_SUBMIT_FLAG_PRESENT;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;
use aero_dxbc::{test_utils as dxbc_test_utils, FourCC};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

const DXBC_VS_MATRIX: &[u8] = include_bytes!("../../aero-d3d11/tests/fixtures/vs_matrix.dxbc");

#[derive(Clone, Copy)]
struct SigParam {
    semantic_name: &'static str,
    semantic_index: u32,
    register: u32,
    mask: u8,
}

fn build_signature_chunk_v0(params: &[SigParam]) -> Vec<u8> {
    let entries: Vec<dxbc_test_utils::SignatureEntryDesc<'_>> = params
        .iter()
        .map(|p| dxbc_test_utils::SignatureEntryDesc {
            semantic_name: p.semantic_name,
            semantic_index: p.semantic_index,
            system_value_type: 0,
            component_type: 0,
            register: p.register,
            mask: p.mask,
            read_write_mask: p.mask,
            stream: 0,
            min_precision: 0,
        })
        .collect();
    dxbc_test_utils::build_signature_chunk_v0(&entries)
}

fn build_ps_solid_rgba_dxbc(rgba: [f32; 4]) -> Vec<u8> {
    // Hand-authored minimal DXBC container: empty ISGN + OSGN(SV_Target0) + SHDR(token stream).
    //
    // Token stream (SM4 subset):
    //   mov o0, l(r,g,b,a)
    //   ret
    let isgn = build_signature_chunk_v0(&[]);
    let osgn = build_signature_chunk_v0(&[SigParam {
        semantic_name: "SV_Target",
        semantic_index: 0,
        register: 0,
        mask: 0x0f,
    }]);

    let version_token = 0x40u32; // ps_4_0
    let mov_token = 0x01u32 | (8u32 << 11);
    let ret_token = 0x3eu32 | (1u32 << 11);

    let dst_o0 = 0x0010_f022u32;
    let imm_vec4 = 0x0000_f042u32;

    let [r, g, b, a] = rgba.map(f32::to_bits);

    let mut tokens = vec![
        version_token,
        0, // length patched below
        mov_token,
        dst_o0,
        0, // o0 index
        imm_vec4,
        r,
        g,
        b,
        a,
        ret_token,
    ];
    tokens[1] = tokens.len() as u32;

    let mut shdr = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        shdr.extend_from_slice(&t.to_le_bytes());
    }

    dxbc_test_utils::build_container_owned(&[
        (FourCC(*b"ISGN"), isgn),
        (FourCC(*b"OSGN"), osgn),
        (FourCC(*b"SHDR"), shdr),
    ])
}

fn fixture_path() -> PathBuf {
    // `CARGO_MANIFEST_DIR` = `.../crates/aero-gpu-trace`
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/aerogpu_cmd_constant_buffer_vs_matrix.aerogputrace")
}

fn ilay_pos3() -> Vec<u8> {
    // Minimal ILAY blob: POSITION0 float3 at slot0 offset0.
    //
    // This matches the input signature of the `vs_matrix.dxbc` fixture.
    //
    // struct aerogpu_input_layout_blob_header (16 bytes)
    // struct aerogpu_input_layout_element_dxgi (28 bytes)
    const MAGIC_ILAY: u32 = 0x5941_4C49; // "ILAY" LE
    const VERSION: u32 = 1;
    const ELEMENT_COUNT: u32 = 1;

    const SEMANTIC_NAME_HASH_POSITION: u32 = 0x7808_E88A; // FNV1a32("POSITION")
    const DXGI_FORMAT_R32G32B32_FLOAT: u32 = 6;

    let mut blob = Vec::with_capacity(44);
    blob.extend_from_slice(&MAGIC_ILAY.to_le_bytes());
    blob.extend_from_slice(&VERSION.to_le_bytes());
    blob.extend_from_slice(&ELEMENT_COUNT.to_le_bytes());
    blob.extend_from_slice(&0u32.to_le_bytes()); // reserved0

    // Element 0
    blob.extend_from_slice(&SEMANTIC_NAME_HASH_POSITION.to_le_bytes());
    blob.extend_from_slice(&0u32.to_le_bytes()); // semantic_index
    blob.extend_from_slice(&DXGI_FORMAT_R32G32B32_FLOAT.to_le_bytes());
    blob.extend_from_slice(&0u32.to_le_bytes()); // input_slot
    blob.extend_from_slice(&0u32.to_le_bytes()); // aligned_byte_offset
    blob.extend_from_slice(&0u32.to_le_bytes()); // input_slot_class (per-vertex)
    blob.extend_from_slice(&0u32.to_le_bytes()); // instance_data_step_rate

    blob
}

fn make_cmd_stream() -> Vec<u8> {
    // Resource handles are arbitrary stable integers.
    const VB: u32 = 1;
    const CB: u32 = 2;
    const RT: u32 = 3;
    const VS: u32 = 10;
    const PS: u32 = 11;
    const IL: u32 = 20;

    // Fullscreen triangle positions (float3).
    let vertices: [f32; 9] = [
        -1.0, -1.0, 0.0, // v0
        -1.0, 3.0, 0.0, // v1
        3.0, -1.0, 0.0, // v2
    ];
    let mut vb_bytes = Vec::with_capacity(vertices.len() * 4);
    for f in vertices {
        vb_bytes.extend_from_slice(&f.to_le_bytes());
    }

    // Identity 4x4 matrix as 16 f32 values (cb0[0..3]).
    let cb_words: [u32; 16] = [
        1.0f32.to_bits(),
        0.0f32.to_bits(),
        0.0f32.to_bits(),
        0.0f32.to_bits(),
        0.0f32.to_bits(),
        1.0f32.to_bits(),
        0.0f32.to_bits(),
        0.0f32.to_bits(),
        0.0f32.to_bits(),
        0.0f32.to_bits(),
        1.0f32.to_bits(),
        0.0f32.to_bits(),
        0.0f32.to_bits(),
        0.0f32.to_bits(),
        0.0f32.to_bits(),
        1.0f32.to_bits(),
    ];
    let mut cb_bytes = Vec::with_capacity(cb_words.len() * 4);
    for w in cb_words {
        cb_bytes.extend_from_slice(&w.to_le_bytes());
    }

    let ilay = ilay_pos3();
    // Use a minimal constant-output pixel shader so this fixture does not require any special
    // fragment-stage builtins (some wgpu backends do not support `@builtin(primitive_index)`).
    let ps_dxbc = build_ps_solid_rgba_dxbc([0.0, 0.0, 0.0, 0.0]);

    let mut w = AerogpuCmdWriter::new();

    w.create_buffer(
        VB,
        AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
        vb_bytes.len() as u64,
        0,
        0,
    );
    w.upload_resource(VB, 0, &vb_bytes);

    w.create_buffer(
        CB,
        AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER,
        cb_bytes.len() as u64,
        0,
        0,
    );
    w.upload_resource(CB, 0, &cb_bytes);

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
    // Clear to red so a missing/misbound constant buffer is likely to leave visible clear color.
    w.clear(AEROGPU_CLEAR_COLOR, [1.0, 0.0, 0.0, 1.0], 1.0, 0);

    w.create_shader_dxbc(VS, AerogpuShaderStage::Vertex, DXBC_VS_MATRIX);
    w.create_shader_dxbc(PS, AerogpuShaderStage::Pixel, &ps_dxbc);
    w.bind_shaders(VS, PS, 0);

    w.create_input_layout(IL, &ilay);
    w.set_input_layout(IL);

    let binding = AerogpuVertexBufferBinding {
        buffer: VB,
        stride_bytes: 12, // float3
        offset_bytes: 0,
        reserved0: 0,
    };
    w.set_vertex_buffers(0, &[binding]);

    let cb_binding = AerogpuConstantBufferBinding {
        buffer: CB,
        offset_bytes: 0,
        size_bytes: cb_bytes.len() as u32,
        reserved0: 0,
    };
    w.set_constant_buffers(AerogpuShaderStage::Vertex, 0, &[cb_binding]);

    w.set_primitive_topology(AerogpuPrimitiveTopology::TriangleList);
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
fn aerogpu_cmd_constant_buffer_vs_matrix_trace_fixture_is_stable() {
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
