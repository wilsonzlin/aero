use aero_gpu_trace::{BlobKind, TraceMeta, TraceReader, TraceWriter};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

const COMMAND_ABI_VERSION: u32 = 1;

// Packet opcodes (see docs/abi/gpu-trace-format.md Appendix A).
const OP_CREATE_BUFFER: u32 = 0x0001;
const OP_UPLOAD_BUFFER: u32 = 0x0002;
const OP_CREATE_SHADER: u32 = 0x0003;
const OP_CREATE_PIPELINE: u32 = 0x0004;
const OP_SET_PIPELINE: u32 = 0x0005;
const OP_SET_VERTEX_BUFFER: u32 = 0x0006;
const OP_SET_VIEWPORT: u32 = 0x0007;
const OP_CLEAR: u32 = 0x0008;
const OP_DRAW: u32 = 0x0009;
const OP_PRESENT: u32 = 0x000A;

fn fixture_path() -> PathBuf {
    // `CARGO_MANIFEST_DIR` = `.../crates/aero-gpu-trace`
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/triangle.aerogputrace")
}

fn packet(opcode: u32, payload: &[u32]) -> Vec<u8> {
    let total_dwords: u32 = 2 + payload.len() as u32;
    let mut bytes = Vec::with_capacity(total_dwords as usize * 4);
    bytes.extend_from_slice(&opcode.to_le_bytes());
    bytes.extend_from_slice(&total_dwords.to_le_bytes());
    for &word in payload {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    bytes
}

fn u64_to_dwords(v: u64) -> [u32; 2] {
    [(v & 0xFFFF_FFFF) as u32, (v >> 32) as u32]
}

fn generate_triangle_trace() -> Vec<u8> {
    let meta = TraceMeta::new("0.0.0-dev", COMMAND_ABI_VERSION);
    let mut writer = TraceWriter::new(Vec::<u8>::new(), &meta).expect("TraceWriter::new");

    writer.begin_frame(0).unwrap();

    // Vertex buffer: a full-screen triangle, interleaved [pos.xy, color.rgba] floats.
    //
    // Positions are in clip space. This is the classic fullscreen-triangle pattern.
    let vertices: [f32; 18] = [
        -1.0, -1.0, 1.0, 0.0, 0.0, 1.0, // v0
        3.0, -1.0, 1.0, 0.0, 0.0, 1.0, // v1
        -1.0, 3.0, 1.0, 0.0, 0.0, 1.0, // v2
    ];
    let mut vertex_bytes = Vec::with_capacity(vertices.len() * 4);
    for f in vertices {
        vertex_bytes.extend_from_slice(&f.to_le_bytes());
    }

    let vertex_blob_id = writer
        .write_blob(BlobKind::BufferData, &vertex_bytes)
        .unwrap();

    // Shaders.
    const GLSL_VS: &str = r#"#version 300 es
precision highp float;
layout(location=0) in vec2 a_position;
layout(location=1) in vec4 a_color;
out vec4 v_color;
void main() {
  v_color = a_color;
  gl_Position = vec4(a_position, 0.0, 1.0);
}
"#;

    const GLSL_FS: &str = r#"#version 300 es
precision highp float;
in vec4 v_color;
out vec4 o_color;
void main() {
  o_color = v_color;
}
"#;

    // The WGSL is recorded for parity with the intended WebGPU backend, but the reference
    // replayer currently uses the GLSL ES source.
    const WGSL_VS: &str = r#"
struct VsIn {
  @location(0) position: vec2<f32>,
  @location(1) color: vec4<f32>,
}
struct VsOut {
  @builtin(position) position: vec4<f32>,
  @location(0) color: vec4<f32>,
}
@vertex
fn vs_main(input: VsIn) -> VsOut {
  var out: VsOut;
  out.position = vec4<f32>(input.position, 0.0, 1.0);
  out.color = input.color;
  return out;
}
"#;

    const WGSL_FS: &str = r#"
@fragment
fn fs_main(@location(0) color: vec4<f32>) -> @location(0) vec4<f32> {
  return color;
}
"#;

    // Dummy DXBC blobs (for analysis tooling). Not used by the replayer.
    const DXBC_STUB: &[u8] = b"DXBCSTUB";

    let glsl_vs_blob = writer
        .write_blob(BlobKind::ShaderGlslEs300, GLSL_VS.as_bytes())
        .unwrap();
    let glsl_fs_blob = writer
        .write_blob(BlobKind::ShaderGlslEs300, GLSL_FS.as_bytes())
        .unwrap();

    let wgsl_vs_blob = writer
        .write_blob(BlobKind::ShaderWgsl, WGSL_VS.as_bytes())
        .unwrap();
    let wgsl_fs_blob = writer
        .write_blob(BlobKind::ShaderWgsl, WGSL_FS.as_bytes())
        .unwrap();

    let dxbc_vs_blob = writer.write_blob(BlobKind::ShaderDxbc, DXBC_STUB).unwrap();
    let dxbc_fs_blob = writer.write_blob(BlobKind::ShaderDxbc, DXBC_STUB).unwrap();

    let buffer_id = 1u32;
    let vs_id = 1u32;
    let fs_id = 2u32;
    let pipeline_id = 1u32;

    let vsize_bytes = vertex_bytes.len() as u32;
    writer
        .write_packet(&packet(OP_CREATE_BUFFER, &[buffer_id, vsize_bytes, 0]))
        .unwrap();

    let [vblob_lo, vblob_hi] = u64_to_dwords(vertex_blob_id);
    writer
        .write_packet(&packet(
            OP_UPLOAD_BUFFER,
            &[buffer_id, 0, vsize_bytes, vblob_lo, vblob_hi],
        ))
        .unwrap();

    let [vs_glsl_lo, vs_glsl_hi] = u64_to_dwords(glsl_vs_blob);
    let [vs_wgsl_lo, vs_wgsl_hi] = u64_to_dwords(wgsl_vs_blob);
    let [vs_dxbc_lo, vs_dxbc_hi] = u64_to_dwords(dxbc_vs_blob);
    writer
        .write_packet(&packet(
            OP_CREATE_SHADER,
            &[
                vs_id, 0, // stage = VS
                vs_glsl_lo, vs_glsl_hi, vs_wgsl_lo, vs_wgsl_hi, vs_dxbc_lo, vs_dxbc_hi,
            ],
        ))
        .unwrap();

    let [fs_glsl_lo, fs_glsl_hi] = u64_to_dwords(glsl_fs_blob);
    let [fs_wgsl_lo, fs_wgsl_hi] = u64_to_dwords(wgsl_fs_blob);
    let [fs_dxbc_lo, fs_dxbc_hi] = u64_to_dwords(dxbc_fs_blob);
    writer
        .write_packet(&packet(
            OP_CREATE_SHADER,
            &[
                fs_id, 1, // stage = FS
                fs_glsl_lo, fs_glsl_hi, fs_wgsl_lo, fs_wgsl_hi, fs_dxbc_lo, fs_dxbc_hi,
            ],
        ))
        .unwrap();

    writer
        .write_packet(&packet(OP_CREATE_PIPELINE, &[pipeline_id, vs_id, fs_id]))
        .unwrap();
    writer
        .write_packet(&packet(OP_SET_PIPELINE, &[pipeline_id]))
        .unwrap();

    // Fixed vertex layout for the reference replayer.
    let stride = 6u32 * 4; // 6 floats
    writer
        .write_packet(&packet(
            OP_SET_VERTEX_BUFFER,
            &[buffer_id, stride, 0, 2 * 4],
        ))
        .unwrap();

    // Viewport is set by the replayer based on the canvas; this record exists to validate
    // that "frame size" can be part of the trace. The replayer treats 0/0 as "use canvas size".
    writer
        .write_packet(&packet(OP_SET_VIEWPORT, &[0, 0]))
        .unwrap();

    writer
        .write_packet(&packet(
            OP_CLEAR,
            &[
                0f32.to_bits(),
                0f32.to_bits(),
                0f32.to_bits(),
                1f32.to_bits(),
            ],
        ))
        .unwrap();

    writer.write_packet(&packet(OP_DRAW, &[3, 0])).unwrap();
    writer.write_packet(&packet(OP_PRESENT, &[])).unwrap();

    writer.present(0).unwrap();

    writer.finish().unwrap()
}

#[test]
fn triangle_trace_fixture_is_stable() {
    let bytes = generate_triangle_trace();

    // Sanity-check: it must parse, and contain exactly one frame.
    let reader = TraceReader::open(Cursor::new(bytes.clone())).expect("TraceReader::open");
    assert_eq!(reader.header.command_abi_version, COMMAND_ABI_VERSION);
    assert_eq!(reader.frame_entries().len(), 1);

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
