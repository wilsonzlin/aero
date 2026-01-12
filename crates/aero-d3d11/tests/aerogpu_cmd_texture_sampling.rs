mod common;

use std::fs;

use aero_d3d11::input_layout::fnv1a_32;
use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::AEROGPU_CMD_STREAM_MAGIC;
use aero_protocol::aerogpu::aerogpu_pci::AEROGPU_ABI_VERSION_U32;
use aero_protocol::aerogpu::aerogpu_ring::AerogpuAllocEntry;

const OPCODE_CREATE_BUFFER: u32 = 0x0100;
const OPCODE_CREATE_TEXTURE2D: u32 = 0x0101;
const OPCODE_RESOURCE_DIRTY_RANGE: u32 = 0x0103;
const OPCODE_UPLOAD_RESOURCE: u32 = 0x0104;
const OPCODE_COPY_TEXTURE2D: u32 = 0x0106;

const OPCODE_CREATE_SHADER_DXBC: u32 = 0x0200;
const OPCODE_BIND_SHADERS: u32 = 0x0202;
const OPCODE_CREATE_INPUT_LAYOUT: u32 = 0x0204;
const OPCODE_SET_INPUT_LAYOUT: u32 = 0x0206;

const OPCODE_SET_RENDER_TARGETS: u32 = 0x0400;
const OPCODE_SET_VIEWPORT: u32 = 0x0401;
const OPCODE_SET_SCISSOR: u32 = 0x0402;

const OPCODE_SET_VERTEX_BUFFERS: u32 = 0x0500;
const OPCODE_SET_PRIMITIVE_TOPOLOGY: u32 = 0x0502;
const OPCODE_SET_TEXTURE: u32 = 0x0510;
const OPCODE_SET_SAMPLER_STATE: u32 = 0x0511;
const OPCODE_CREATE_SAMPLER: u32 = 0x0520;
const OPCODE_SET_SAMPLERS: u32 = 0x0522;
const OPCODE_SET_CONSTANT_BUFFERS: u32 = 0x0523;

const OPCODE_CLEAR: u32 = 0x0600;
const OPCODE_DRAW: u32 = 0x0601;
const OPCODE_PRESENT: u32 = 0x0700;

const AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER: u32 = 1u32 << 0;
const AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER: u32 = 1u32 << 2;
const AEROGPU_RESOURCE_USAGE_TEXTURE: u32 = 1u32 << 3;
const AEROGPU_RESOURCE_USAGE_RENDER_TARGET: u32 = 1u32 << 4;

const AEROGPU_FORMAT_R8G8B8A8_UNORM: u32 = 3;
// ABI 1.2+ extension (see `enum aerogpu_format` in `drivers/aerogpu/protocol/aerogpu_pci.h`).
const AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB: u32 = 9;
// ABI 1.2+ extension (see `enum aerogpu_format` in `drivers/aerogpu/protocol/aerogpu_pci.h`).
const AEROGPU_FORMAT_BC1_RGBA_UNORM_SRGB: u32 = 65;
// ABI 1.2+ extension (see `enum aerogpu_format` in `drivers/aerogpu/protocol/aerogpu_pci.h`).
const AEROGPU_FORMAT_BC2_RGBA_UNORM_SRGB: u32 = 67;
// ABI 1.2+ extension (see `enum aerogpu_format` in `drivers/aerogpu/protocol/aerogpu_pci.h`).
const AEROGPU_FORMAT_BC3_RGBA_UNORM_SRGB: u32 = 69;
// ABI 1.2+ extension (see `enum aerogpu_format` in `drivers/aerogpu/protocol/aerogpu_pci.h`).
const AEROGPU_FORMAT_BC7_RGBA_UNORM_SRGB: u32 = 71;

const AEROGPU_CLEAR_COLOR: u32 = 1u32 << 0;

const AEROGPU_TOPOLOGY_TRIANGLELIST: u32 = 4;

fn load_fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    fs::read(&path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"))
}

fn align4(len: usize) -> usize {
    (len + 3) & !3
}

fn begin_cmd(stream: &mut Vec<u8>, opcode: u32) -> usize {
    let start = stream.len();
    stream.extend_from_slice(&opcode.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // size placeholder
    start
}

fn end_cmd(stream: &mut [u8], start: usize) {
    let size = (stream.len() - start) as u32;
    stream[start + 4..start + 8].copy_from_slice(&size.to_le_bytes());
    assert_eq!(size % 4, 0, "command not 4-byte aligned");
}

fn build_dxbc(chunks: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let chunk_count: u32 = chunks
        .len()
        .try_into()
        .expect("chunk count should fit in u32");

    let header_size = 4 + 16 + 4 + 4 + 4 + 4 * chunks.len();
    let mut offsets = Vec::with_capacity(chunks.len());
    let mut cursor = header_size;
    for (_fourcc, data) in chunks {
        offsets.push(cursor);
        cursor += 8 + align4(data.len());
    }

    let total_size: u32 = cursor.try_into().expect("dxbc size should fit in u32");

    let mut bytes = Vec::with_capacity(cursor);
    bytes.extend_from_slice(b"DXBC");
    bytes.extend_from_slice(&[0u8; 16]); // checksum (ignored)
    bytes.extend_from_slice(&1u32.to_le_bytes()); // "one"
    bytes.extend_from_slice(&total_size.to_le_bytes());
    bytes.extend_from_slice(&chunk_count.to_le_bytes());
    for offset in offsets {
        bytes.extend_from_slice(&(offset as u32).to_le_bytes());
    }

    for (fourcc, data) in chunks {
        bytes.extend_from_slice(fourcc);
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(data);
        bytes.resize(bytes.len() + (align4(data.len()) - data.len()), 0);
    }
    bytes
}

#[derive(Clone, Copy)]
struct SigParam {
    semantic_name: &'static str,
    semantic_index: u32,
    register: u32,
    mask: u8,
}

fn build_signature_chunk(params: &[SigParam]) -> Vec<u8> {
    // Mirrors `aero_d3d11::signature::parse_signature_chunk` expectations.
    let mut out = Vec::new();
    out.extend_from_slice(&(params.len() as u32).to_le_bytes()); // param_count
    out.extend_from_slice(&8u32.to_le_bytes()); // param_offset

    let entry_size = 24usize;
    let table_start = out.len();
    out.resize(table_start + params.len() * entry_size, 0);

    for (i, p) in params.iter().enumerate() {
        let semantic_name_offset = out.len() as u32;
        out.extend_from_slice(p.semantic_name.as_bytes());
        out.push(0);
        while out.len() % 4 != 0 {
            out.push(0);
        }

        let base = table_start + i * entry_size;
        out[base..base + 4].copy_from_slice(&semantic_name_offset.to_le_bytes());
        out[base + 4..base + 8].copy_from_slice(&p.semantic_index.to_le_bytes());
        out[base + 8..base + 12].copy_from_slice(&0u32.to_le_bytes()); // system_value_type
        out[base + 12..base + 16].copy_from_slice(&0u32.to_le_bytes()); // component_type
        out[base + 16..base + 20].copy_from_slice(&p.register.to_le_bytes());
        out[base + 20] = p.mask;
        out[base + 21] = p.mask; // read_write_mask
        out[base + 22] = 0; // stream
        out[base + 23] = 0; // min_precision
    }

    out
}

fn build_ps_sample_t0_s0_dxbc(u: f32, v: f32) -> Vec<u8> {
    // Hand-authored minimal DXBC container: ISGN(empty) + OSGN(SV_Target0) +
    // SHDR(token stream).
    //
    // Token stream is SM4-ish and only relies on `aero_d3d11`'s subset decoder:
    //   sample o0, l(u, v, 0, 0), t0, s0
    //   ret
    //
    // The shader samples with an immediate coordinate and does not consume any inputs, but we
    // still include an ISGN chunk so the signature-driven translator is used.
    let isgn = build_signature_chunk(&[]);
    let osgn = build_signature_chunk(&[SigParam {
        semantic_name: "SV_Target",
        semantic_index: 0,
        register: 0,
        mask: 0x0f,
    }]);

    let version_token = 0x40u32; // ps_4_0
    let sample_opcode_token = 0x45u32 | (12u32 << 11);
    let ret_token = 0x3eu32 | (1u32 << 11);

    let dst_o0 = 0x0010_f022u32;
    let imm_vec4 = 0x0000_f042u32;
    let t0 = 0x0010_0072u32;
    let s0 = 0x0010_0062u32;

    let u = u.to_bits();
    let v = v.to_bits();
    let mut tokens = vec![
        version_token,
        0, // length patched below
        sample_opcode_token,
        dst_o0,
        0, // o0 index
        imm_vec4,
        u,
        v,
        0,
        0,
        t0,
        0, // t0 index
        s0,
        0, // s0 index
        ret_token,
    ];
    tokens[1] = tokens.len() as u32;

    let mut shdr = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        shdr.extend_from_slice(&t.to_le_bytes());
    }

    build_dxbc(&[(*b"ISGN", isgn), (*b"OSGN", osgn), (*b"SHDR", shdr)])
}

fn build_ps_ld_t0_mip_dxbc(mip_level: u32) -> Vec<u8> {
    // Minimal SM4-ish PS token stream:
    //   ld o0, l(0, 0, 0, 0), t0
    //   ret
    //
    // `ld` uses an immediate coordinate and does not consume any inputs, but we still include an
    // ISGN chunk so the signature-driven translator is used.
    let isgn = build_signature_chunk(&[]);
    let osgn = build_signature_chunk(&[SigParam {
        semantic_name: "SV_Target",
        semantic_index: 0,
        register: 0,
        mask: 0x0f,
    }]);

    let version_token = 0x40u32; // ps_4_0
    let ld_opcode_token = 0x4cu32 | (10u32 << 11);
    let ret_token = 0x3eu32 | (1u32 << 11);

    let dst_o0 = 0x0010_f022u32;
    let imm_vec4 = 0x0000_f042u32;
    let t0 = 0x0010_0072u32;

    let mut tokens = vec![
        version_token,
        0, // length patched below
        ld_opcode_token,
        dst_o0,
        0, // o0 index
        imm_vec4,
        0,
        0,
        // The `ld` translation expects the mip level to be carried as an integer value in the
        // raw bits of the coordinate operand (see `emit_src_vec4_i32`).
        mip_level,
        0,
        t0,
        0, // t0 index
        ret_token,
    ];
    tokens[1] = tokens.len() as u32;

    let mut shdr = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        shdr.extend_from_slice(&t.to_le_bytes());
    }

    build_dxbc(&[(*b"ISGN", isgn), (*b"OSGN", osgn), (*b"SHDR", shdr)])
}

fn build_ps_ld_t0_f32_coord_dxbc(x: f32, y: f32, mip: f32) -> Vec<u8> {
    // Minimal SM4-ish PS token stream:
    //   ld o0, l(x, y, mip, 0), t0
    //   ret
    //
    // Unlike `build_ps_ld_t0_mip_dxbc`, which stores the mip as raw integer bits, this helper stores
    // coordinates/LOD as numeric float values. This exercises the `textureLoad` argument recovery
    // logic that prefers `i32(f32)` when the lane looks like an exact integer.
    let isgn = build_signature_chunk(&[]);
    let osgn = build_signature_chunk(&[SigParam {
        semantic_name: "SV_Target",
        semantic_index: 0,
        register: 0,
        mask: 0x0f,
    }]);

    let version_token = 0x40u32; // ps_4_0
    let ld_opcode_token = 0x4cu32 | (10u32 << 11);
    let ret_token = 0x3eu32 | (1u32 << 11);

    let dst_o0 = 0x0010_f022u32;
    let imm_vec4 = 0x0000_f042u32;
    let t0 = 0x0010_0072u32;

    let mut tokens = vec![
        version_token,
        0, // length patched below
        ld_opcode_token,
        dst_o0,
        0, // o0 index
        imm_vec4,
        x.to_bits(),
        y.to_bits(),
        mip.to_bits(),
        0.0f32.to_bits(),
        t0,
        0, // t0 index
        ret_token,
    ];
    tokens[1] = tokens.len() as u32;

    let mut shdr = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        shdr.extend_from_slice(&t.to_le_bytes());
    }

    build_dxbc(&[(*b"ISGN", isgn), (*b"OSGN", osgn), (*b"SHDR", shdr)])
}

fn build_ps_constant_color_dxbc(color: [f32; 4]) -> Vec<u8> {
    // Minimal SM4-ish PS token stream:
    //   mov o0, l(r, g, b, a)
    //   ret
    let isgn = build_signature_chunk(&[]);
    let osgn = build_signature_chunk(&[SigParam {
        semantic_name: "SV_Target",
        semantic_index: 0,
        register: 0,
        mask: 0x0f,
    }]);

    let version_token = 0x40u32; // ps_4_0
    let mov_opcode_token = 0x01u32 | (8u32 << 11);
    let ret_token = 0x3eu32 | (1u32 << 11);

    let dst_o0 = 0x0010_f022u32;
    let imm_vec4 = 0x0000_f042u32;

    let mut tokens = vec![
        version_token,
        0, // length patched below
        mov_opcode_token,
        dst_o0,
        0, // o0 index
        imm_vec4,
        color[0].to_bits(),
        color[1].to_bits(),
        color[2].to_bits(),
        color[3].to_bits(),
        ret_token,
    ];
    tokens[1] = tokens.len() as u32;

    let mut shdr = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        shdr.extend_from_slice(&t.to_le_bytes());
    }

    build_dxbc(&[(*b"ISGN", isgn), (*b"OSGN", osgn), (*b"SHDR", shdr)])
}

fn build_ilay_pos3_only() -> Vec<u8> {
    // `struct aerogpu_input_layout_blob_header` + one DXGI element.
    let mut out = Vec::new();
    out.extend_from_slice(&0x5941_4c49u32.to_le_bytes()); // "ILAY"
    out.extend_from_slice(&1u32.to_le_bytes()); // version
    out.extend_from_slice(&1u32.to_le_bytes()); // element_count
    out.extend_from_slice(&0u32.to_le_bytes()); // reserved0

    out.extend_from_slice(&fnv1a_32(b"POSITION").to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // semantic_index
    out.extend_from_slice(&6u32.to_le_bytes()); // DXGI_FORMAT_R32G32B32_FLOAT
    out.extend_from_slice(&0u32.to_le_bytes()); // input_slot
    out.extend_from_slice(&0u32.to_le_bytes()); // aligned_byte_offset
    out.extend_from_slice(&0u32.to_le_bytes()); // input_slot_class (per-vertex)
    out.extend_from_slice(&0u32.to_le_bytes()); // instance_data_step_rate
    out
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    pos: [f32; 3],
    color: [f32; 4],
}

#[test]
fn aerogpu_cmd_renders_with_texture_sampling_ps() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB: u32 = 1;
        const RT: u32 = 2;
        const TEX: u32 = 3;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vertices = [
            Vertex {
                pos: [-1.0, -1.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [-1.0, 3.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [3.0, -1.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);
        let vb_size = vb_bytes.len() as u64;

        let mut guest_mem = VecGuestMemory::new(0x1000);
        let alloc_id = 1u32;
        let alloc_gpa = 0x100u64;
        guest_mem.write(alloc_gpa, vb_bytes).unwrap();

        let allocs = [AerogpuAllocEntry {
            alloc_id,
            flags: 0,
            gpa: alloc_gpa,
            size_bytes: vb_size,
            reserved0: 0,
        }];

        let dxbc_vs = load_fixture("vs_passthrough.dxbc");
        let dxbc_ps = build_ps_sample_t0_s0_dxbc(0.5, 0.5);
        let ilay = load_fixture("ilay_pos3_color.bin");

        // 2x2 uniform green texture.
        let texel = [0u8, 255u8, 0u8, 255u8];
        let tex_data: Vec<u8> = texel.repeat(4);

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (VB)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_BUFFER);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&vb_size.to_le_bytes());
        stream.extend_from_slice(&alloc_id.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // RESOURCE_DIRTY_RANGE (full VB)
        let start = begin_cmd(&mut stream, OPCODE_RESOURCE_DIRTY_RANGE);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&vb_size.to_le_bytes()); // size_bytes
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM.to_le_bytes());
        stream.extend_from_slice(&4u32.to_le_bytes()); // width
        stream.extend_from_slice(&4u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id (host alloc)
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (SRV texture)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM.to_le_bytes());
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&2u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id (host alloc)
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (SRV texture bytes)
        let start = begin_cmd(&mut stream, OPCODE_UPLOAD_RESOURCE);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(tex_data.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(&tex_data);
        stream.resize(stream.len() + (align4(tex_data.len()) - tex_data.len()), 0);
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, OPCODE_SET_RENDER_TARGETS);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // VIEWPORT 0..4
        let start = begin_cmd(&mut stream, OPCODE_SET_VIEWPORT);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        end_cmd(&mut stream, start);

        // SCISSOR 0..4
        let start = begin_cmd(&mut stream, OPCODE_SET_SCISSOR);
        stream.extend_from_slice(&0i32.to_le_bytes());
        stream.extend_from_slice(&0i32.to_le_bytes());
        stream.extend_from_slice(&4i32.to_le_bytes());
        stream.extend_from_slice(&4i32.to_le_bytes());
        end_cmd(&mut stream, start);

        // CLEAR (black)
        let start = begin_cmd(&mut stream, OPCODE_CLEAR);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SHADER_DXBC);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(dxbc_vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&dxbc_vs);
        stream.resize(stream.len() + (align4(dxbc_vs.len()) - dxbc_vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SHADER_DXBC);
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // stage = pixel
        stream.extend_from_slice(&(dxbc_ps.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&dxbc_ps);
        stream.resize(stream.len() + (align4(dxbc_ps.len()) - dxbc_ps.len()), 0);
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, OPCODE_BIND_SHADERS);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_INPUT_LAYOUT (ILAY blob)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_INPUT_LAYOUT);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ilay.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&ilay);
        stream.resize(stream.len() + (align4(ilay.len()) - ilay.len()), 0);
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, OPCODE_SET_INPUT_LAYOUT);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS
        let start = begin_cmd(&mut stream, OPCODE_SET_VERTEX_BUFFERS);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&(std::mem::size_of::<Vertex>() as u32).to_le_bytes()); // stride
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, OPCODE_SET_PRIMITIVE_TOPOLOGY);
        stream.extend_from_slice(&AEROGPU_TOPOLOGY_TRIANGLELIST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0 = TEX)
        let start = begin_cmd(&mut stream, OPCODE_SET_TEXTURE);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot = 0
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_SAMPLER_STATE (PS s0 = default)
        let start = begin_cmd(&mut stream, OPCODE_SET_SAMPLER_STATE);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot = 0
        stream.extend_from_slice(&0u32.to_le_bytes()); // state
        stream.extend_from_slice(&0u32.to_le_bytes()); // value
        end_cmd(&mut stream, start);

        // DRAW
        let start = begin_cmd(&mut stream, OPCODE_DRAW);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // PRESENT
        let start = begin_cmd(&mut stream, OPCODE_PRESENT);
        stream.extend_from_slice(&0u32.to_le_bytes()); // scanout_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[8..12].copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .unwrap();
        exec.poll_wait();

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), 4 * 4 * 4);
        for px in pixels.chunks_exact(4) {
            assert_eq!(px, &[0, 255, 0, 255]);
        }
    });
}

#[test]
fn aerogpu_cmd_texture_sampling_linearizes_srgb_textures() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB: u32 = 1;
        const RT: u32 = 2;
        const TEX: u32 = 3;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vertices = [
            Vertex {
                pos: [-1.0, -1.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [-1.0, 3.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [3.0, -1.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);
        let vb_size = vb_bytes.len() as u64;

        let mut guest_mem = VecGuestMemory::new(0x1000);
        let alloc_id = 1u32;
        let alloc_gpa = 0x100u64;
        guest_mem.write(alloc_gpa, vb_bytes).unwrap();

        let allocs = [AerogpuAllocEntry {
            alloc_id,
            flags: 0,
            gpa: alloc_gpa,
            size_bytes: vb_size,
            reserved0: 0,
        }];

        let dxbc_vs = load_fixture("vs_passthrough.dxbc");
        let dxbc_ps = build_ps_sample_t0_s0_dxbc(0.5, 0.5);
        let ilay = load_fixture("ilay_pos3_color.bin");

        // 2x2 uniform sRGB texture (mid red). When sampled, the GPU should convert sRGB->linear
        // before writing into the UNORM render target.
        //
        // sRGB 0x80 (~0.502) maps to linear ~0.216, which quantizes to ~55 in 8-bit UNORM.
        let texel = [128u8, 0u8, 0u8, 255u8];
        let tex_data: Vec<u8> = texel.repeat(4);

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (VB)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_BUFFER);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&vb_size.to_le_bytes());
        stream.extend_from_slice(&alloc_id.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // RESOURCE_DIRTY_RANGE (full VB)
        let start = begin_cmd(&mut stream, OPCODE_RESOURCE_DIRTY_RANGE);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&vb_size.to_le_bytes()); // size_bytes
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT) - UNORM so we can observe sRGB->linear conversion on output bytes.
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM.to_le_bytes());
        stream.extend_from_slice(&4u32.to_le_bytes()); // width
        stream.extend_from_slice(&4u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id (host alloc)
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (SRV texture) - SRGB
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB.to_le_bytes());
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&2u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id (host alloc)
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (SRV texture, full upload)
        let start = begin_cmd(&mut stream, OPCODE_UPLOAD_RESOURCE);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(tex_data.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(&tex_data);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SHADER_DXBC);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(dxbc_vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&dxbc_vs);
        stream.resize(stream.len() + (align4(dxbc_vs.len()) - dxbc_vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SHADER_DXBC);
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // stage = pixel
        stream.extend_from_slice(&(dxbc_ps.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&dxbc_ps);
        stream.resize(stream.len() + (align4(dxbc_ps.len()) - dxbc_ps.len()), 0);
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, OPCODE_BIND_SHADERS);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, OPCODE_CREATE_INPUT_LAYOUT);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ilay.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&ilay);
        stream.resize(stream.len() + (align4(ilay.len()) - ilay.len()), 0);
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, OPCODE_SET_INPUT_LAYOUT);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, OPCODE_SET_RENDER_TARGETS);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // VIEWPORT 0..4
        let start = begin_cmd(&mut stream, OPCODE_SET_VIEWPORT);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // SCISSOR 0..4
        let start = begin_cmd(&mut stream, OPCODE_SET_SCISSOR);
        stream.extend_from_slice(&0i32.to_le_bytes()); // x
        stream.extend_from_slice(&0i32.to_le_bytes()); // y
        stream.extend_from_slice(&4i32.to_le_bytes()); // w
        stream.extend_from_slice(&4i32.to_le_bytes()); // h
        end_cmd(&mut stream, start);

        // CLEAR (black)
        let start = begin_cmd(&mut stream, OPCODE_CLEAR);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS
        let start = begin_cmd(&mut stream, OPCODE_SET_VERTEX_BUFFERS);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&(core::mem::size_of::<Vertex>() as u32).to_le_bytes()); // stride
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, OPCODE_SET_PRIMITIVE_TOPOLOGY);
        stream.extend_from_slice(&AEROGPU_TOPOLOGY_TRIANGLELIST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0)
        let start = begin_cmd(&mut stream, OPCODE_SET_TEXTURE);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot = 0
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_SAMPLER_STATE (PS s0 = default)
        let start = begin_cmd(&mut stream, OPCODE_SET_SAMPLER_STATE);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot = 0
        stream.extend_from_slice(&0u32.to_le_bytes()); // state
        stream.extend_from_slice(&0u32.to_le_bytes()); // value
        end_cmd(&mut stream, start);

        // DRAW
        let start = begin_cmd(&mut stream, OPCODE_DRAW);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // PRESENT
        let start = begin_cmd(&mut stream, OPCODE_PRESENT);
        stream.extend_from_slice(&0u32.to_le_bytes()); // scanout_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[8..12].copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .unwrap();
        exec.poll_wait();

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), 4 * 4 * 4);
        for px in pixels.chunks_exact(4) {
            // Expect sRGB->linear conversion: sRGB(128) ~= linear(55).
            assert!((px[0] as i32 - 55).abs() <= 2, "r={}", px[0]);
            assert!(px[1] <= 2, "g={}", px[1]);
            assert!(px[2] <= 2, "b={}", px[2]);
            assert!((px[3] as i32 - 255).abs() <= 2, "a={}", px[3]);
        }
    });
}

#[test]
fn aerogpu_cmd_clear_srgb_render_target_encodes_linear_values() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const RT: u32 = 1;

        let mut guest_mem = VecGuestMemory::new(0x1000);

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_TEXTURE2D (RT) - SRGB so we can observe linear->sRGB encoding on output bytes.
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB.to_le_bytes());
        stream.extend_from_slice(&4u32.to_le_bytes()); // width
        stream.extend_from_slice(&4u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id (host alloc)
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, OPCODE_SET_RENDER_TARGETS);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // VIEWPORT 0..4
        let start = begin_cmd(&mut stream, OPCODE_SET_VIEWPORT);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // SCISSOR 0..4
        let start = begin_cmd(&mut stream, OPCODE_SET_SCISSOR);
        stream.extend_from_slice(&0i32.to_le_bytes()); // x
        stream.extend_from_slice(&0i32.to_le_bytes()); // y
        stream.extend_from_slice(&4i32.to_le_bytes()); // w
        stream.extend_from_slice(&4i32.to_le_bytes()); // h
        end_cmd(&mut stream, start);

        // CLEAR (linear 0.5 gray). Should store as ~188 in sRGB.
        let start = begin_cmd(&mut stream, OPCODE_CLEAR);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        stream.extend_from_slice(&0.5f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0.5f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0.5f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // PRESENT
        let start = begin_cmd(&mut stream, OPCODE_PRESENT);
        stream.extend_from_slice(&0u32.to_le_bytes()); // scanout_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[8..12].copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream(&stream, None, &mut guest_mem).unwrap();
        exec.poll_wait();

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), 4 * 4 * 4);
        for px in pixels.chunks_exact(4) {
            assert!(
                (px[0] as i32 - 188).abs() <= 3 && px[0] == px[1] && px[1] == px[2],
                "expected encoded ~188 gray, got {px:?}"
            );
            assert!((px[3] as i32 - 255).abs() <= 2, "a={}", px[3]);
        }
    });
}

#[test]
fn aerogpu_cmd_texture_sampling_srgb_to_srgb_roundtrips() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB: u32 = 1;
        const RT: u32 = 2;
        const TEX: u32 = 3;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vertices = [
            Vertex {
                pos: [-1.0, -1.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [-1.0, 3.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [3.0, -1.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);
        let vb_size = vb_bytes.len() as u64;

        let mut guest_mem = VecGuestMemory::new(0x1000);
        let alloc_id = 1u32;
        let alloc_gpa = 0x100u64;
        guest_mem.write(alloc_gpa, vb_bytes).unwrap();

        let allocs = [AerogpuAllocEntry {
            alloc_id,
            flags: 0,
            gpa: alloc_gpa,
            size_bytes: vb_size,
            reserved0: 0,
        }];

        let dxbc_vs = load_fixture("vs_passthrough.dxbc");
        let dxbc_ps = build_ps_sample_t0_s0_dxbc(0.5, 0.5);
        let ilay = load_fixture("ilay_pos3_color.bin");

        // 2x2 uniform sRGB texture (mid red). Sampling from an sRGB texture returns linear values,
        // and writing to an sRGB render target should re-encode to sRGB, yielding the same stored
        // byte (~128) if both conversions happen exactly once.
        let texel = [128u8, 0u8, 0u8, 255u8];
        let tex_data: Vec<u8> = texel.repeat(4);

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (VB)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_BUFFER);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&vb_size.to_le_bytes());
        stream.extend_from_slice(&alloc_id.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // RESOURCE_DIRTY_RANGE (full VB)
        let start = begin_cmd(&mut stream, OPCODE_RESOURCE_DIRTY_RANGE);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&vb_size.to_le_bytes()); // size_bytes
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT) - SRGB so we can observe linear->sRGB encoding on output bytes.
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB.to_le_bytes());
        stream.extend_from_slice(&4u32.to_le_bytes()); // width
        stream.extend_from_slice(&4u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id (host alloc)
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (SRV texture) - SRGB
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB.to_le_bytes());
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&2u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id (host alloc)
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (SRV texture, full upload)
        let start = begin_cmd(&mut stream, OPCODE_UPLOAD_RESOURCE);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(tex_data.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(&tex_data);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SHADER_DXBC);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(dxbc_vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&dxbc_vs);
        stream.resize(stream.len() + (align4(dxbc_vs.len()) - dxbc_vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SHADER_DXBC);
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // stage = pixel
        stream.extend_from_slice(&(dxbc_ps.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&dxbc_ps);
        stream.resize(stream.len() + (align4(dxbc_ps.len()) - dxbc_ps.len()), 0);
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, OPCODE_BIND_SHADERS);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, OPCODE_CREATE_INPUT_LAYOUT);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ilay.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&ilay);
        stream.resize(stream.len() + (align4(ilay.len()) - ilay.len()), 0);
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, OPCODE_SET_INPUT_LAYOUT);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, OPCODE_SET_RENDER_TARGETS);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // VIEWPORT 0..4
        let start = begin_cmd(&mut stream, OPCODE_SET_VIEWPORT);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // SCISSOR 0..4
        let start = begin_cmd(&mut stream, OPCODE_SET_SCISSOR);
        stream.extend_from_slice(&0i32.to_le_bytes()); // x
        stream.extend_from_slice(&0i32.to_le_bytes()); // y
        stream.extend_from_slice(&4i32.to_le_bytes()); // w
        stream.extend_from_slice(&4i32.to_le_bytes()); // h
        end_cmd(&mut stream, start);

        // CLEAR (black)
        let start = begin_cmd(&mut stream, OPCODE_CLEAR);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS
        let start = begin_cmd(&mut stream, OPCODE_SET_VERTEX_BUFFERS);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&(core::mem::size_of::<Vertex>() as u32).to_le_bytes()); // stride
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, OPCODE_SET_PRIMITIVE_TOPOLOGY);
        stream.extend_from_slice(&AEROGPU_TOPOLOGY_TRIANGLELIST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0)
        let start = begin_cmd(&mut stream, OPCODE_SET_TEXTURE);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot = 0
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_SAMPLER_STATE (PS s0 = default)
        let start = begin_cmd(&mut stream, OPCODE_SET_SAMPLER_STATE);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot = 0
        stream.extend_from_slice(&0u32.to_le_bytes()); // state
        stream.extend_from_slice(&0u32.to_le_bytes()); // value
        end_cmd(&mut stream, start);

        // DRAW
        let start = begin_cmd(&mut stream, OPCODE_DRAW);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // PRESENT
        let start = begin_cmd(&mut stream, OPCODE_PRESENT);
        stream.extend_from_slice(&0u32.to_le_bytes()); // scanout_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[8..12].copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .unwrap();
        exec.poll_wait();

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), 4 * 4 * 4);
        for px in pixels.chunks_exact(4) {
            // Expect decode+encode roundtrip: sRGB(128) should remain ~128, not become ~188.
            assert!((px[0] as i32 - 128).abs() <= 2, "r={}", px[0]);
            assert!(px[1] <= 2, "g={}", px[1]);
            assert!(px[2] <= 2, "b={}", px[2]);
            assert!((px[3] as i32 - 255).abs() <= 2, "a={}", px[3]);
        }
    });
}

#[test]
fn aerogpu_cmd_texture_sampling_linearizes_bc1_srgb_textures() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB: u32 = 1;
        const RT: u32 = 2;
        const TEX: u32 = 3;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vertices = [
            Vertex {
                pos: [-1.0, -1.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [-1.0, 3.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [3.0, -1.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);
        let vb_size = vb_bytes.len() as u64;

        let mut guest_mem = VecGuestMemory::new(0x1000);
        let alloc_id = 1u32;
        let alloc_gpa = 0x100u64;
        guest_mem.write(alloc_gpa, vb_bytes).unwrap();

        let allocs = [AerogpuAllocEntry {
            alloc_id,
            flags: 0,
            gpa: alloc_gpa,
            size_bytes: vb_size,
            reserved0: 0,
        }];

        let dxbc_vs = load_fixture("vs_passthrough.dxbc");
        let dxbc_ps = build_ps_sample_t0_s0_dxbc(0.5, 0.5);
        let ilay = load_fixture("ilay_pos3_color.bin");

        // A single BC1 block with color0=white, color1=black, and all indices set to 2 (the 2/3
        // white entry in 4-color mode). This decompresses to an sRGB value of 170.
        //
        // When sampled from an sRGB BC1 texture, 170 should decode to linear ~0.402 and store to an
        // UNORM render target as ~103.
        let bc1_srgb_gray_170: [u8; 8] = [
            0xff, 0xff, // color0 (white)
            0x00, 0x00, // color1 (black)
            0xaa, 0xaa, 0xaa, 0xaa, // indices: all 2
        ];

        // Validate the block layout matches the expected pattern (helps keep the expected output
        // meaningful if the fixture changes).
        let decompressed = aero_gpu::decompress_bc1_rgba8(4, 4, &bc1_srgb_gray_170);
        for px in decompressed.chunks_exact(4) {
            assert_eq!(px, &[170, 170, 170, 255]);
        }

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (VB)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_BUFFER);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&vb_size.to_le_bytes());
        stream.extend_from_slice(&alloc_id.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // RESOURCE_DIRTY_RANGE (full VB)
        let start = begin_cmd(&mut stream, OPCODE_RESOURCE_DIRTY_RANGE);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&vb_size.to_le_bytes()); // size_bytes
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT) - UNORM so we can observe sRGB->linear conversion on output bytes.
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM.to_le_bytes());
        stream.extend_from_slice(&4u32.to_le_bytes()); // width
        stream.extend_from_slice(&4u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id (host alloc)
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (SRV texture) - BC1 SRGB
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_FORMAT_BC1_RGBA_UNORM_SRGB.to_le_bytes());
        stream.extend_from_slice(&4u32.to_le_bytes()); // width
        stream.extend_from_slice(&4u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id (host alloc)
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (SRV texture)
        let start = begin_cmd(&mut stream, OPCODE_UPLOAD_RESOURCE);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(bc1_srgb_gray_170.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(&bc1_srgb_gray_170);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SHADER_DXBC);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(dxbc_vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&dxbc_vs);
        stream.resize(stream.len() + (align4(dxbc_vs.len()) - dxbc_vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SHADER_DXBC);
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // stage = pixel
        stream.extend_from_slice(&(dxbc_ps.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&dxbc_ps);
        stream.resize(stream.len() + (align4(dxbc_ps.len()) - dxbc_ps.len()), 0);
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, OPCODE_BIND_SHADERS);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, OPCODE_CREATE_INPUT_LAYOUT);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ilay.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&ilay);
        stream.resize(stream.len() + (align4(ilay.len()) - ilay.len()), 0);
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, OPCODE_SET_INPUT_LAYOUT);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, OPCODE_SET_RENDER_TARGETS);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // VIEWPORT 0..4
        let start = begin_cmd(&mut stream, OPCODE_SET_VIEWPORT);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // SCISSOR 0..4
        let start = begin_cmd(&mut stream, OPCODE_SET_SCISSOR);
        stream.extend_from_slice(&0i32.to_le_bytes()); // x
        stream.extend_from_slice(&0i32.to_le_bytes()); // y
        stream.extend_from_slice(&4i32.to_le_bytes()); // w
        stream.extend_from_slice(&4i32.to_le_bytes()); // h
        end_cmd(&mut stream, start);

        // CLEAR (black)
        let start = begin_cmd(&mut stream, OPCODE_CLEAR);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS
        let start = begin_cmd(&mut stream, OPCODE_SET_VERTEX_BUFFERS);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&(core::mem::size_of::<Vertex>() as u32).to_le_bytes()); // stride
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, OPCODE_SET_PRIMITIVE_TOPOLOGY);
        stream.extend_from_slice(&AEROGPU_TOPOLOGY_TRIANGLELIST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0)
        let start = begin_cmd(&mut stream, OPCODE_SET_TEXTURE);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot = 0
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_SAMPLER_STATE (PS s0 = default)
        let start = begin_cmd(&mut stream, OPCODE_SET_SAMPLER_STATE);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot = 0
        stream.extend_from_slice(&0u32.to_le_bytes()); // state
        stream.extend_from_slice(&0u32.to_le_bytes()); // value
        end_cmd(&mut stream, start);

        // DRAW
        let start = begin_cmd(&mut stream, OPCODE_DRAW);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // PRESENT
        let start = begin_cmd(&mut stream, OPCODE_PRESENT);
        stream.extend_from_slice(&0u32.to_le_bytes()); // scanout_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[8..12].copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .unwrap();
        exec.poll_wait();

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), 4 * 4 * 4);
        for px in pixels.chunks_exact(4) {
            assert!(
                (px[0] as i32 - 103).abs() <= 5 && px[0] == px[1] && px[1] == px[2],
                "expected decoded ~103 gray, got {px:?}"
            );
            assert!((px[3] as i32 - 255).abs() <= 2, "a={}", px[3]);
        }
    });
}

#[test]
fn aerogpu_cmd_texture_sampling_linearizes_bc2_srgb_textures() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB: u32 = 1;
        const RT: u32 = 2;
        const TEX: u32 = 3;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vertices = [
            Vertex {
                pos: [-1.0, -1.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [-1.0, 3.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [3.0, -1.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);
        let vb_size = vb_bytes.len() as u64;

        let mut guest_mem = VecGuestMemory::new(0x1000);
        let alloc_id = 1u32;
        let alloc_gpa = 0x100u64;
        guest_mem.write(alloc_gpa, vb_bytes).unwrap();

        let allocs = [AerogpuAllocEntry {
            alloc_id,
            flags: 0,
            gpa: alloc_gpa,
            size_bytes: vb_size,
            reserved0: 0,
        }];

        let dxbc_vs = load_fixture("vs_passthrough.dxbc");
        let dxbc_ps = build_ps_sample_t0_s0_dxbc(0.5, 0.5);
        let ilay = load_fixture("ilay_pos3_color.bin");

        // A single BC2 block with:
        // - alpha bits all 0xF (255)
        // - BC1-like color block: color0=white, color1=black, indices all 2 -> sRGB gray 170
        //
        // When sampled from an sRGB BC2 texture, 170 should decode to linear ~0.402 and store to an
        // UNORM render target as ~103.
        let bc2_srgb_gray_170: [u8; 16] = [
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // alpha (64-bit LE, all 0xF)
            0xff, 0xff, // color0 (white)
            0x00, 0x00, // color1 (black)
            0xaa, 0xaa, 0xaa, 0xaa, // color indices: all 2
        ];

        let decompressed = aero_gpu::decompress_bc2_rgba8(4, 4, &bc2_srgb_gray_170);
        for px in decompressed.chunks_exact(4) {
            assert_eq!(px, &[170, 170, 170, 255]);
        }

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (VB)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_BUFFER);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&vb_size.to_le_bytes());
        stream.extend_from_slice(&alloc_id.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // RESOURCE_DIRTY_RANGE (full VB)
        let start = begin_cmd(&mut stream, OPCODE_RESOURCE_DIRTY_RANGE);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&vb_size.to_le_bytes()); // size_bytes
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT) - UNORM so we can observe sRGB->linear conversion on output bytes.
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM.to_le_bytes());
        stream.extend_from_slice(&4u32.to_le_bytes()); // width
        stream.extend_from_slice(&4u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id (host alloc)
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (SRV texture) - BC2 SRGB
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_FORMAT_BC2_RGBA_UNORM_SRGB.to_le_bytes());
        stream.extend_from_slice(&4u32.to_le_bytes()); // width
        stream.extend_from_slice(&4u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id (host alloc)
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (SRV texture)
        let start = begin_cmd(&mut stream, OPCODE_UPLOAD_RESOURCE);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(bc2_srgb_gray_170.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(&bc2_srgb_gray_170);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SHADER_DXBC);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(dxbc_vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&dxbc_vs);
        stream.resize(stream.len() + (align4(dxbc_vs.len()) - dxbc_vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SHADER_DXBC);
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // stage = pixel
        stream.extend_from_slice(&(dxbc_ps.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&dxbc_ps);
        stream.resize(stream.len() + (align4(dxbc_ps.len()) - dxbc_ps.len()), 0);
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, OPCODE_BIND_SHADERS);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, OPCODE_CREATE_INPUT_LAYOUT);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ilay.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&ilay);
        stream.resize(stream.len() + (align4(ilay.len()) - ilay.len()), 0);
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, OPCODE_SET_INPUT_LAYOUT);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, OPCODE_SET_RENDER_TARGETS);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // VIEWPORT 0..4
        let start = begin_cmd(&mut stream, OPCODE_SET_VIEWPORT);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // SCISSOR 0..4
        let start = begin_cmd(&mut stream, OPCODE_SET_SCISSOR);
        stream.extend_from_slice(&0i32.to_le_bytes()); // x
        stream.extend_from_slice(&0i32.to_le_bytes()); // y
        stream.extend_from_slice(&4i32.to_le_bytes()); // w
        stream.extend_from_slice(&4i32.to_le_bytes()); // h
        end_cmd(&mut stream, start);

        // CLEAR (black)
        let start = begin_cmd(&mut stream, OPCODE_CLEAR);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS
        let start = begin_cmd(&mut stream, OPCODE_SET_VERTEX_BUFFERS);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&(core::mem::size_of::<Vertex>() as u32).to_le_bytes()); // stride
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, OPCODE_SET_PRIMITIVE_TOPOLOGY);
        stream.extend_from_slice(&AEROGPU_TOPOLOGY_TRIANGLELIST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0)
        let start = begin_cmd(&mut stream, OPCODE_SET_TEXTURE);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot = 0
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_SAMPLER_STATE (PS s0 = default)
        let start = begin_cmd(&mut stream, OPCODE_SET_SAMPLER_STATE);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot = 0
        stream.extend_from_slice(&0u32.to_le_bytes()); // state
        stream.extend_from_slice(&0u32.to_le_bytes()); // value
        end_cmd(&mut stream, start);

        // DRAW
        let start = begin_cmd(&mut stream, OPCODE_DRAW);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // PRESENT
        let start = begin_cmd(&mut stream, OPCODE_PRESENT);
        stream.extend_from_slice(&0u32.to_le_bytes()); // scanout_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[8..12].copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .unwrap();
        exec.poll_wait();

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), 4 * 4 * 4);
        for px in pixels.chunks_exact(4) {
            assert!(
                (px[0] as i32 - 103).abs() <= 5 && px[0] == px[1] && px[1] == px[2],
                "expected decoded ~103 gray, got {px:?}"
            );
            assert!((px[3] as i32 - 255).abs() <= 2, "a={}", px[3]);
        }
    });
}

#[test]
fn aerogpu_cmd_texture_sampling_linearizes_bc3_srgb_textures() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB: u32 = 1;
        const RT: u32 = 2;
        const TEX: u32 = 3;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vertices = [
            Vertex {
                pos: [-1.0, -1.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [-1.0, 3.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [3.0, -1.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);
        let vb_size = vb_bytes.len() as u64;

        let mut guest_mem = VecGuestMemory::new(0x1000);
        let alloc_id = 1u32;
        let alloc_gpa = 0x100u64;
        guest_mem.write(alloc_gpa, vb_bytes).unwrap();

        let allocs = [AerogpuAllocEntry {
            alloc_id,
            flags: 0,
            gpa: alloc_gpa,
            size_bytes: vb_size,
            reserved0: 0,
        }];

        let dxbc_vs = load_fixture("vs_passthrough.dxbc");
        let dxbc_ps = build_ps_sample_t0_s0_dxbc(0.5, 0.5);
        let ilay = load_fixture("ilay_pos3_color.bin");

        // A single BC3 block with:
        // - alpha0=255, alpha1=0, indices all 0 -> alpha 255
        // - color0=white, color1=black, indices all 2 -> sRGB gray 170
        //
        // When sampled from an sRGB BC3 texture, 170 should decode to linear ~0.402 and store to an
        // UNORM render target as ~103.
        let bc3_srgb_gray_170: [u8; 16] = [
            0xff, 0x00, // alpha0, alpha1
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // alpha indices (48-bit LE)
            0xff, 0xff, // color0 (white)
            0x00, 0x00, // color1 (black)
            0xaa, 0xaa, 0xaa, 0xaa, // color indices: all 2
        ];

        let decompressed = aero_gpu::decompress_bc3_rgba8(4, 4, &bc3_srgb_gray_170);
        for px in decompressed.chunks_exact(4) {
            assert_eq!(px, &[170, 170, 170, 255]);
        }

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (VB)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_BUFFER);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&vb_size.to_le_bytes());
        stream.extend_from_slice(&alloc_id.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // RESOURCE_DIRTY_RANGE (full VB)
        let start = begin_cmd(&mut stream, OPCODE_RESOURCE_DIRTY_RANGE);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&vb_size.to_le_bytes()); // size_bytes
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT) - UNORM so we can observe sRGB->linear conversion on output bytes.
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM.to_le_bytes());
        stream.extend_from_slice(&4u32.to_le_bytes()); // width
        stream.extend_from_slice(&4u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id (host alloc)
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (SRV texture) - BC3 SRGB
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_FORMAT_BC3_RGBA_UNORM_SRGB.to_le_bytes());
        stream.extend_from_slice(&4u32.to_le_bytes()); // width
        stream.extend_from_slice(&4u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id (host alloc)
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (SRV texture)
        let start = begin_cmd(&mut stream, OPCODE_UPLOAD_RESOURCE);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(bc3_srgb_gray_170.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(&bc3_srgb_gray_170);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SHADER_DXBC);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(dxbc_vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&dxbc_vs);
        stream.resize(stream.len() + (align4(dxbc_vs.len()) - dxbc_vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SHADER_DXBC);
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // stage = pixel
        stream.extend_from_slice(&(dxbc_ps.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&dxbc_ps);
        stream.resize(stream.len() + (align4(dxbc_ps.len()) - dxbc_ps.len()), 0);
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, OPCODE_BIND_SHADERS);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, OPCODE_CREATE_INPUT_LAYOUT);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ilay.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&ilay);
        stream.resize(stream.len() + (align4(ilay.len()) - ilay.len()), 0);
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, OPCODE_SET_INPUT_LAYOUT);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, OPCODE_SET_RENDER_TARGETS);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // VIEWPORT 0..4
        let start = begin_cmd(&mut stream, OPCODE_SET_VIEWPORT);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // SCISSOR 0..4
        let start = begin_cmd(&mut stream, OPCODE_SET_SCISSOR);
        stream.extend_from_slice(&0i32.to_le_bytes()); // x
        stream.extend_from_slice(&0i32.to_le_bytes()); // y
        stream.extend_from_slice(&4i32.to_le_bytes()); // w
        stream.extend_from_slice(&4i32.to_le_bytes()); // h
        end_cmd(&mut stream, start);

        // CLEAR (black)
        let start = begin_cmd(&mut stream, OPCODE_CLEAR);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS
        let start = begin_cmd(&mut stream, OPCODE_SET_VERTEX_BUFFERS);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&(core::mem::size_of::<Vertex>() as u32).to_le_bytes()); // stride
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, OPCODE_SET_PRIMITIVE_TOPOLOGY);
        stream.extend_from_slice(&AEROGPU_TOPOLOGY_TRIANGLELIST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0)
        let start = begin_cmd(&mut stream, OPCODE_SET_TEXTURE);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot = 0
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_SAMPLER_STATE (PS s0 = default)
        let start = begin_cmd(&mut stream, OPCODE_SET_SAMPLER_STATE);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot = 0
        stream.extend_from_slice(&0u32.to_le_bytes()); // state
        stream.extend_from_slice(&0u32.to_le_bytes()); // value
        end_cmd(&mut stream, start);

        // DRAW
        let start = begin_cmd(&mut stream, OPCODE_DRAW);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // PRESENT
        let start = begin_cmd(&mut stream, OPCODE_PRESENT);
        stream.extend_from_slice(&0u32.to_le_bytes()); // scanout_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[8..12].copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .unwrap();
        exec.poll_wait();

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), 4 * 4 * 4);
        for px in pixels.chunks_exact(4) {
            assert!(
                (px[0] as i32 - 103).abs() <= 5 && px[0] == px[1] && px[1] == px[2],
                "expected decoded ~103 gray, got {px:?}"
            );
            assert!((px[3] as i32 - 255).abs() <= 2, "a={}", px[3]);
        }
    });
}

#[test]
fn aerogpu_cmd_texture_sampling_linearizes_bc7_srgb_textures() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB: u32 = 1;
        const RT: u32 = 2;
        const TEX: u32 = 3;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vertices = [
            Vertex {
                pos: [-1.0, -1.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [-1.0, 3.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [3.0, -1.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);
        let vb_size = vb_bytes.len() as u64;

        let mut guest_mem = VecGuestMemory::new(0x1000);
        let alloc_id = 1u32;
        let alloc_gpa = 0x100u64;
        guest_mem.write(alloc_gpa, vb_bytes).unwrap();

        let allocs = [AerogpuAllocEntry {
            alloc_id,
            flags: 0,
            gpa: alloc_gpa,
            size_bytes: vb_size,
            reserved0: 0,
        }];

        let dxbc_vs = load_fixture("vs_passthrough.dxbc");
        // Use `ld` (textureLoad) so we can deterministically fetch texel (0,0) even though this BC7
        // block isn't uniform.
        let dxbc_ps = build_ps_ld_t0_mip_dxbc(0);
        let ilay = load_fixture("ilay_pos3_color.bin");

        // A single BC7 block where texel (0,0) decodes to sRGB gray 170.
        //
        // When sampled from an sRGB BC7 texture, 170 should decode to linear ~0.402 and store to an
        // UNORM render target as ~103.
        let bc7_srgb_texel00_gray_170: [u8; 16] = [
            0x0d, 0x9b, 0x60, 0x7f, 0x13, 0x62, 0x68, 0x33, 0x8f, 0xde, 0x1d, 0x56, 0x21, 0xdd,
            0x30, 0xc1,
        ];
        let decompressed = aero_gpu::decompress_bc7_rgba8(4, 4, &bc7_srgb_texel00_gray_170);
        assert_eq!(&decompressed[0..4], &[170, 170, 170, 255]);

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (VB)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_BUFFER);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&vb_size.to_le_bytes());
        stream.extend_from_slice(&alloc_id.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // RESOURCE_DIRTY_RANGE (full VB)
        let start = begin_cmd(&mut stream, OPCODE_RESOURCE_DIRTY_RANGE);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&vb_size.to_le_bytes()); // size_bytes
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT) - UNORM so we can observe sRGB->linear conversion on output bytes.
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM.to_le_bytes());
        stream.extend_from_slice(&4u32.to_le_bytes()); // width
        stream.extend_from_slice(&4u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id (host alloc)
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (SRV texture) - BC7 SRGB
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_FORMAT_BC7_RGBA_UNORM_SRGB.to_le_bytes());
        stream.extend_from_slice(&4u32.to_le_bytes()); // width
        stream.extend_from_slice(&4u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id (host alloc)
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (SRV texture)
        let start = begin_cmd(&mut stream, OPCODE_UPLOAD_RESOURCE);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(bc7_srgb_texel00_gray_170.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(&bc7_srgb_texel00_gray_170);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SHADER_DXBC);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(dxbc_vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&dxbc_vs);
        stream.resize(stream.len() + (align4(dxbc_vs.len()) - dxbc_vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SHADER_DXBC);
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // stage = pixel
        stream.extend_from_slice(&(dxbc_ps.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&dxbc_ps);
        stream.resize(stream.len() + (align4(dxbc_ps.len()) - dxbc_ps.len()), 0);
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, OPCODE_BIND_SHADERS);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, OPCODE_CREATE_INPUT_LAYOUT);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ilay.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&ilay);
        stream.resize(stream.len() + (align4(ilay.len()) - ilay.len()), 0);
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, OPCODE_SET_INPUT_LAYOUT);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, OPCODE_SET_RENDER_TARGETS);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // VIEWPORT 0..4
        let start = begin_cmd(&mut stream, OPCODE_SET_VIEWPORT);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // x
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes()); // width
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes()); // height
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
        end_cmd(&mut stream, start);

        // SCISSOR 0..4
        let start = begin_cmd(&mut stream, OPCODE_SET_SCISSOR);
        stream.extend_from_slice(&0i32.to_le_bytes()); // x
        stream.extend_from_slice(&0i32.to_le_bytes()); // y
        stream.extend_from_slice(&4i32.to_le_bytes()); // w
        stream.extend_from_slice(&4i32.to_le_bytes()); // h
        end_cmd(&mut stream, start);

        // CLEAR (black)
        let start = begin_cmd(&mut stream, OPCODE_CLEAR);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS
        let start = begin_cmd(&mut stream, OPCODE_SET_VERTEX_BUFFERS);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&(core::mem::size_of::<Vertex>() as u32).to_le_bytes()); // stride
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, OPCODE_SET_PRIMITIVE_TOPOLOGY);
        stream.extend_from_slice(&AEROGPU_TOPOLOGY_TRIANGLELIST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0)
        let start = begin_cmd(&mut stream, OPCODE_SET_TEXTURE);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot = 0
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // DRAW
        let start = begin_cmd(&mut stream, OPCODE_DRAW);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // PRESENT
        let start = begin_cmd(&mut stream, OPCODE_PRESENT);
        stream.extend_from_slice(&0u32.to_le_bytes()); // scanout_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[8..12].copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .unwrap();
        exec.poll_wait();

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), 4 * 4 * 4);
        for px in pixels.chunks_exact(4) {
            assert!(
                (px[0] as i32 - 103).abs() <= 5 && px[0] == px[1] && px[1] == px[2],
                "expected decoded ~103 gray, got {px:?}"
            );
            assert!((px[3] as i32 - 255).abs() <= 2, "a={}", px[3]);
        }
    });
}

#[test]
fn aerogpu_cmd_set_samplers_binds_created_sampler() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB: u32 = 1;
        const RT: u32 = 2;
        const TEX: u32 = 3;
        const SAMP: u32 = 4;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vertices = [
            Vertex {
                pos: [-1.0, -1.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [-1.0, 3.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [3.0, -1.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);
        let vb_size = vb_bytes.len() as u64;

        let mut guest_mem = VecGuestMemory::new(0x1000);
        let alloc_id = 1u32;
        let alloc_gpa = 0x100u64;
        guest_mem.write(alloc_gpa, vb_bytes).unwrap();

        let allocs = [AerogpuAllocEntry {
            alloc_id,
            flags: 0,
            gpa: alloc_gpa,
            size_bytes: vb_size,
            reserved0: 0,
        }];

        let dxbc_vs = load_fixture("vs_passthrough.dxbc");
        // Pick a coordinate that lies between texel centers so linear filtering blends.
        let dxbc_ps = build_ps_sample_t0_s0_dxbc(0.4, 0.25);
        let ilay = load_fixture("ilay_pos3_color.bin");

        // 2x2 texture: red on the left, green on the right (both rows identical).
        let red = [255u8, 0u8, 0u8, 255u8];
        let green = [0u8, 255u8, 0u8, 255u8];
        let tex_data: Vec<u8> = [red, green, red, green].into_iter().flatten().collect();

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (VB)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_BUFFER);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&vb_size.to_le_bytes());
        stream.extend_from_slice(&alloc_id.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // RESOURCE_DIRTY_RANGE (full VB)
        let start = begin_cmd(&mut stream, OPCODE_RESOURCE_DIRTY_RANGE);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&vb_size.to_le_bytes()); // size_bytes
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM.to_le_bytes());
        stream.extend_from_slice(&4u32.to_le_bytes()); // width
        stream.extend_from_slice(&4u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id (host alloc)
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (SRV texture)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM.to_le_bytes());
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&2u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id (host alloc)
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (SRV texture bytes)
        let start = begin_cmd(&mut stream, OPCODE_UPLOAD_RESOURCE);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(tex_data.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(&tex_data);
        stream.resize(stream.len() + (align4(tex_data.len()) - tex_data.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SAMPLER (nearest + clamp)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SAMPLER);
        stream.extend_from_slice(&SAMP.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // filter = nearest
        stream.extend_from_slice(&0u32.to_le_bytes()); // address_u = clamp
        stream.extend_from_slice(&0u32.to_le_bytes()); // address_v = clamp
        stream.extend_from_slice(&0u32.to_le_bytes()); // address_w = clamp
        end_cmd(&mut stream, start);

        // SET_SAMPLERS (PS s0 = SAMP)
        let start = begin_cmd(&mut stream, OPCODE_SET_SAMPLERS);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot = 0
        stream.extend_from_slice(&1u32.to_le_bytes()); // sampler_count = 1
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&SAMP.to_le_bytes());
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, OPCODE_SET_RENDER_TARGETS);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // VIEWPORT 0..4
        let start = begin_cmd(&mut stream, OPCODE_SET_VIEWPORT);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        end_cmd(&mut stream, start);

        // SCISSOR 0..4
        let start = begin_cmd(&mut stream, OPCODE_SET_SCISSOR);
        stream.extend_from_slice(&0i32.to_le_bytes());
        stream.extend_from_slice(&0i32.to_le_bytes());
        stream.extend_from_slice(&4i32.to_le_bytes());
        stream.extend_from_slice(&4i32.to_le_bytes());
        end_cmd(&mut stream, start);

        // CLEAR (black)
        let start = begin_cmd(&mut stream, OPCODE_CLEAR);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SHADER_DXBC);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(dxbc_vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&dxbc_vs);
        stream.resize(stream.len() + (align4(dxbc_vs.len()) - dxbc_vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SHADER_DXBC);
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // stage = pixel
        stream.extend_from_slice(&(dxbc_ps.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&dxbc_ps);
        stream.resize(stream.len() + (align4(dxbc_ps.len()) - dxbc_ps.len()), 0);
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, OPCODE_BIND_SHADERS);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_INPUT_LAYOUT (ILAY blob)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_INPUT_LAYOUT);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ilay.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&ilay);
        stream.resize(stream.len() + (align4(ilay.len()) - ilay.len()), 0);
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, OPCODE_SET_INPUT_LAYOUT);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS
        let start = begin_cmd(&mut stream, OPCODE_SET_VERTEX_BUFFERS);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&(std::mem::size_of::<Vertex>() as u32).to_le_bytes()); // stride
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, OPCODE_SET_PRIMITIVE_TOPOLOGY);
        stream.extend_from_slice(&AEROGPU_TOPOLOGY_TRIANGLELIST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0 = TEX)
        let start = begin_cmd(&mut stream, OPCODE_SET_TEXTURE);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot = 0
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // DRAW
        let start = begin_cmd(&mut stream, OPCODE_DRAW);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // PRESENT
        let start = begin_cmd(&mut stream, OPCODE_PRESENT);
        stream.extend_from_slice(&0u32.to_le_bytes()); // scanout_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[8..12].copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .unwrap();
        exec.poll_wait();

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), 4 * 4 * 4);
        for px in pixels.chunks_exact(4) {
            assert_eq!(px, &[255, 0, 0, 255]);
        }
    });
}

#[test]
fn aerogpu_cmd_renders_with_texture_load_ps() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB: u32 = 1;
        const RT: u32 = 2;
        const TEX: u32 = 3;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vertices = [
            Vertex {
                pos: [-1.0, -1.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [-1.0, 3.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [3.0, -1.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);
        let vb_size = vb_bytes.len() as u64;

        let mut guest_mem = VecGuestMemory::new(0x1000);
        let alloc_id = 1u32;
        let alloc_gpa = 0x100u64;
        guest_mem.write(alloc_gpa, vb_bytes).unwrap();

        let allocs = [AerogpuAllocEntry {
            alloc_id,
            flags: 0,
            gpa: alloc_gpa,
            size_bytes: vb_size,
            reserved0: 0,
        }];

        let dxbc_vs = load_fixture("vs_passthrough.dxbc");
        let dxbc_ps = load_fixture("ps_ld.dxbc");
        let ilay = load_fixture("ilay_pos3_color.bin");

        // 2x2 uniform green texture. `ld` should not require a sampler binding.
        let texel = [0u8, 255u8, 0u8, 255u8];
        let tex_data: Vec<u8> = texel.repeat(4);

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (VB)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_BUFFER);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&vb_size.to_le_bytes());
        stream.extend_from_slice(&alloc_id.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // RESOURCE_DIRTY_RANGE (full VB)
        let start = begin_cmd(&mut stream, OPCODE_RESOURCE_DIRTY_RANGE);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&vb_size.to_le_bytes()); // size_bytes
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM.to_le_bytes());
        stream.extend_from_slice(&4u32.to_le_bytes()); // width
        stream.extend_from_slice(&4u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id (host alloc)
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (SRV texture)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM.to_le_bytes());
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&2u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id (host alloc)
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (SRV texture bytes)
        let start = begin_cmd(&mut stream, OPCODE_UPLOAD_RESOURCE);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(tex_data.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(&tex_data);
        stream.resize(stream.len() + (align4(tex_data.len()) - tex_data.len()), 0);
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, OPCODE_SET_RENDER_TARGETS);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // VIEWPORT 0..4
        let start = begin_cmd(&mut stream, OPCODE_SET_VIEWPORT);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        end_cmd(&mut stream, start);

        // SCISSOR 0..4
        let start = begin_cmd(&mut stream, OPCODE_SET_SCISSOR);
        stream.extend_from_slice(&0i32.to_le_bytes());
        stream.extend_from_slice(&0i32.to_le_bytes());
        stream.extend_from_slice(&4i32.to_le_bytes());
        stream.extend_from_slice(&4i32.to_le_bytes());
        end_cmd(&mut stream, start);

        // CLEAR (black)
        let start = begin_cmd(&mut stream, OPCODE_CLEAR);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SHADER_DXBC);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(dxbc_vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&dxbc_vs);
        stream.resize(stream.len() + (align4(dxbc_vs.len()) - dxbc_vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SHADER_DXBC);
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // stage = pixel
        stream.extend_from_slice(&(dxbc_ps.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&dxbc_ps);
        stream.resize(stream.len() + (align4(dxbc_ps.len()) - dxbc_ps.len()), 0);
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, OPCODE_BIND_SHADERS);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_INPUT_LAYOUT (ILAY blob)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_INPUT_LAYOUT);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ilay.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&ilay);
        stream.resize(stream.len() + (align4(ilay.len()) - ilay.len()), 0);
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, OPCODE_SET_INPUT_LAYOUT);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS
        let start = begin_cmd(&mut stream, OPCODE_SET_VERTEX_BUFFERS);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&(std::mem::size_of::<Vertex>() as u32).to_le_bytes()); // stride
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, OPCODE_SET_PRIMITIVE_TOPOLOGY);
        stream.extend_from_slice(&AEROGPU_TOPOLOGY_TRIANGLELIST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0 = TEX)
        let start = begin_cmd(&mut stream, OPCODE_SET_TEXTURE);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot = 0
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // DRAW
        let start = begin_cmd(&mut stream, OPCODE_DRAW);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // PRESENT
        let start = begin_cmd(&mut stream, OPCODE_PRESENT);
        stream.extend_from_slice(&0u32.to_le_bytes()); // scanout_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[8..12].copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .unwrap();
        exec.poll_wait();

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), 4 * 4 * 4);
        for px in pixels.chunks_exact(4) {
            assert_eq!(px, &[0, 255, 0, 255]);
        }
    });
}

#[test]
fn aerogpu_cmd_renders_with_texture_load_nonzero_coord_ps() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB: u32 = 1;
        const RT: u32 = 2;
        const TEX: u32 = 3;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vertices = [
            Vertex {
                pos: [-1.0, -1.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [-1.0, 3.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [3.0, -1.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);
        let vb_size = vb_bytes.len() as u64;

        let mut guest_mem = VecGuestMemory::new(0x1000);
        let alloc_id = 1u32;
        let alloc_gpa = 0x100u64;
        guest_mem.write(alloc_gpa, vb_bytes).unwrap();

        let allocs = [AerogpuAllocEntry {
            alloc_id,
            flags: 0,
            gpa: alloc_gpa,
            size_bytes: vb_size,
            reserved0: 0,
        }];

        let dxbc_vs = load_fixture("vs_passthrough.dxbc");
        // Force a non-zero coordinate encoded as a numeric float (1.0f32), not raw integer bits.
        // This catches regressions where `ld` coordinates are recovered via `bitcast<i32>` only.
        let dxbc_ps = build_ps_ld_t0_f32_coord_dxbc(1.0, 0.0, 0.0);
        let ilay = load_fixture("ilay_pos3_color.bin");

        // 2x2 texture with distinct texels.
        //
        // Layout (row-major):
        //   (0,0)=red, (1,0)=green
        //   (0,1)=blue, (1,1)=white
        let tex_data: Vec<u8> = vec![
            255, 0, 0, 255, // (0,0)
            0, 255, 0, 255, // (1,0)
            0, 0, 255, 255, // (0,1)
            255, 255, 255, 255, // (1,1)
        ];

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (VB)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_BUFFER);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&vb_size.to_le_bytes());
        stream.extend_from_slice(&alloc_id.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // RESOURCE_DIRTY_RANGE (full VB)
        let start = begin_cmd(&mut stream, OPCODE_RESOURCE_DIRTY_RANGE);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&vb_size.to_le_bytes()); // size_bytes
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM.to_le_bytes());
        stream.extend_from_slice(&4u32.to_le_bytes()); // width
        stream.extend_from_slice(&4u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id (host alloc)
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (SRV texture)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM.to_le_bytes());
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&2u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id (host alloc)
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (SRV texture bytes)
        let start = begin_cmd(&mut stream, OPCODE_UPLOAD_RESOURCE);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(tex_data.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(&tex_data);
        stream.resize(stream.len() + (align4(tex_data.len()) - tex_data.len()), 0);
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, OPCODE_SET_RENDER_TARGETS);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // VIEWPORT 0..4
        let start = begin_cmd(&mut stream, OPCODE_SET_VIEWPORT);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        end_cmd(&mut stream, start);

        // SCISSOR 0..4
        let start = begin_cmd(&mut stream, OPCODE_SET_SCISSOR);
        stream.extend_from_slice(&0i32.to_le_bytes());
        stream.extend_from_slice(&0i32.to_le_bytes());
        stream.extend_from_slice(&4i32.to_le_bytes());
        stream.extend_from_slice(&4i32.to_le_bytes());
        end_cmd(&mut stream, start);

        // CLEAR (black)
        let start = begin_cmd(&mut stream, OPCODE_CLEAR);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SHADER_DXBC);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(dxbc_vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&dxbc_vs);
        stream.resize(stream.len() + (align4(dxbc_vs.len()) - dxbc_vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SHADER_DXBC);
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // stage = pixel
        stream.extend_from_slice(&(dxbc_ps.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&dxbc_ps);
        stream.resize(stream.len() + (align4(dxbc_ps.len()) - dxbc_ps.len()), 0);
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, OPCODE_BIND_SHADERS);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_INPUT_LAYOUT (ILAY blob)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_INPUT_LAYOUT);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ilay.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&ilay);
        stream.resize(stream.len() + (align4(ilay.len()) - ilay.len()), 0);
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, OPCODE_SET_INPUT_LAYOUT);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS
        let start = begin_cmd(&mut stream, OPCODE_SET_VERTEX_BUFFERS);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&(std::mem::size_of::<Vertex>() as u32).to_le_bytes()); // stride
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, OPCODE_SET_PRIMITIVE_TOPOLOGY);
        stream.extend_from_slice(&AEROGPU_TOPOLOGY_TRIANGLELIST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0 = TEX)
        let start = begin_cmd(&mut stream, OPCODE_SET_TEXTURE);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot = 0
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // DRAW
        let start = begin_cmd(&mut stream, OPCODE_DRAW);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // PRESENT
        let start = begin_cmd(&mut stream, OPCODE_PRESENT);
        stream.extend_from_slice(&0u32.to_le_bytes()); // scanout_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[8..12].copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .unwrap();
        exec.poll_wait();

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), 4 * 4 * 4);
        for px in pixels.chunks_exact(4) {
            assert_eq!(px, &[0, 255, 0, 255]);
        }
    });
}

#[test]
fn aerogpu_cmd_renders_with_allocation_backed_texture_mip() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB: u32 = 1;
        const RT: u32 = 2;
        const TEX: u32 = 3;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vertices = [
            Vertex {
                pos: [-1.0, -1.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [-1.0, 3.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [3.0, -1.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);
        let vb_size = vb_bytes.len() as u64;

        let mut guest_mem = VecGuestMemory::new(0x1000);
        let alloc_id = 1u32;
        let alloc_gpa = 0x100u64;

        // Backing blob layout is mip0 bytes first then mip1 (row_pitch is shifted per mip).
        // mip0: 2x2 (row_pitch=8, 2 rows) => 16 bytes
        // mip1: 1x1 (row_pitch=4, 1 row) => 4 bytes
        let mip0_texel = [255u8, 0u8, 0u8, 255u8];
        let mip1_texel = [0u8, 255u8, 0u8, 255u8];
        let mut tex_backing = Vec::new();
        tex_backing.extend_from_slice(&mip0_texel.repeat(4));
        tex_backing.extend_from_slice(&mip1_texel);
        guest_mem.write(alloc_gpa, &tex_backing).unwrap();

        let allocs = [AerogpuAllocEntry {
            alloc_id,
            flags: 0,
            gpa: alloc_gpa,
            size_bytes: tex_backing.len() as u64,
            reserved0: 0,
        }];

        let dxbc_vs = load_fixture("vs_passthrough.dxbc");
        let dxbc_ps = build_ps_ld_t0_mip_dxbc(1);
        let ilay = load_fixture("ilay_pos3_color.bin");

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (VB, host alloc)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_BUFFER);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&vb_size.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id (host alloc)
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (VB bytes)
        let start = begin_cmd(&mut stream, OPCODE_UPLOAD_RESOURCE);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(vb_bytes);
        stream.resize(stream.len() + (align4(vb_bytes.len()) - vb_bytes.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM.to_le_bytes());
        stream.extend_from_slice(&4u32.to_le_bytes()); // width
        stream.extend_from_slice(&4u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id (host alloc)
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (SRV texture, allocation-backed with mip 0+1)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM.to_le_bytes());
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&2u32.to_le_bytes()); // height
        stream.extend_from_slice(&2u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&8u32.to_le_bytes()); // row_pitch_bytes (mip0)
        stream.extend_from_slice(&alloc_id.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // RESOURCE_DIRTY_RANGE (SRV texture)
        let start = begin_cmd(&mut stream, OPCODE_RESOURCE_DIRTY_RANGE);
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(tex_backing.len() as u64).to_le_bytes()); // size_bytes
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, OPCODE_SET_RENDER_TARGETS);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // VIEWPORT 0..4
        let start = begin_cmd(&mut stream, OPCODE_SET_VIEWPORT);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        end_cmd(&mut stream, start);

        // SCISSOR 0..4
        let start = begin_cmd(&mut stream, OPCODE_SET_SCISSOR);
        stream.extend_from_slice(&0i32.to_le_bytes());
        stream.extend_from_slice(&0i32.to_le_bytes());
        stream.extend_from_slice(&4i32.to_le_bytes());
        stream.extend_from_slice(&4i32.to_le_bytes());
        end_cmd(&mut stream, start);

        // CLEAR (black)
        let start = begin_cmd(&mut stream, OPCODE_CLEAR);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SHADER_DXBC);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(dxbc_vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&dxbc_vs);
        stream.resize(stream.len() + (align4(dxbc_vs.len()) - dxbc_vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SHADER_DXBC);
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // stage = pixel
        stream.extend_from_slice(&(dxbc_ps.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&dxbc_ps);
        stream.resize(stream.len() + (align4(dxbc_ps.len()) - dxbc_ps.len()), 0);
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, OPCODE_BIND_SHADERS);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_INPUT_LAYOUT (ILAY blob)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_INPUT_LAYOUT);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ilay.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&ilay);
        stream.resize(stream.len() + (align4(ilay.len()) - ilay.len()), 0);
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, OPCODE_SET_INPUT_LAYOUT);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS
        let start = begin_cmd(&mut stream, OPCODE_SET_VERTEX_BUFFERS);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&(std::mem::size_of::<Vertex>() as u32).to_le_bytes()); // stride
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, OPCODE_SET_PRIMITIVE_TOPOLOGY);
        stream.extend_from_slice(&AEROGPU_TOPOLOGY_TRIANGLELIST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_TEXTURE (PS t0 = TEX)
        let start = begin_cmd(&mut stream, OPCODE_SET_TEXTURE);
        stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
        stream.extend_from_slice(&0u32.to_le_bytes()); // slot = 0
        stream.extend_from_slice(&TEX.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // DRAW
        let start = begin_cmd(&mut stream, OPCODE_DRAW);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // PRESENT
        let start = begin_cmd(&mut stream, OPCODE_PRESENT);
        stream.extend_from_slice(&0u32.to_le_bytes()); // scanout_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[8..12].copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .unwrap();
        exec.poll_wait();

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), 4 * 4 * 4);
        for px in pixels.chunks_exact(4) {
            assert_eq!(px, &[0, 255, 0, 255]);
        }
    });
}

#[test]
fn aerogpu_cmd_copy_texture2d_reads_allocation_backed_mip() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const SRC: u32 = 1;
        const DST: u32 = 2;

        let mut guest_mem = VecGuestMemory::new(0x1000);
        let alloc_id = 1u32;
        let alloc_gpa = 0x100u64;

        let mip0_texel = [255u8, 0u8, 0u8, 255u8];
        let mip1_texel = [0u8, 255u8, 0u8, 255u8];
        let mut tex_backing = Vec::new();
        tex_backing.extend_from_slice(&mip0_texel.repeat(4));
        tex_backing.extend_from_slice(&mip1_texel);
        guest_mem.write(alloc_gpa, &tex_backing).unwrap();

        let allocs = [AerogpuAllocEntry {
            alloc_id,
            flags: 0,
            gpa: alloc_gpa,
            size_bytes: tex_backing.len() as u64,
            reserved0: 0,
        }];

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_TEXTURE2D (SRC: allocation-backed with mip 0+1)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&SRC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM.to_le_bytes());
        stream.extend_from_slice(&2u32.to_le_bytes()); // width
        stream.extend_from_slice(&2u32.to_le_bytes()); // height
        stream.extend_from_slice(&2u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&8u32.to_le_bytes()); // row_pitch_bytes (mip0)
        stream.extend_from_slice(&alloc_id.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // RESOURCE_DIRTY_RANGE (SRC)
        let start = begin_cmd(&mut stream, OPCODE_RESOURCE_DIRTY_RANGE);
        stream.extend_from_slice(&SRC.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(tex_backing.len() as u64).to_le_bytes()); // size_bytes
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (DST: 1x1 host alloc)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&DST.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id (host alloc)
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // COPY_TEXTURE2D: SRC mip1 -> DST mip0
        let start = begin_cmd(&mut stream, OPCODE_COPY_TEXTURE2D);
        stream.extend_from_slice(&DST.to_le_bytes());
        stream.extend_from_slice(&SRC.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_mip_level
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_array_layer
        stream.extend_from_slice(&1u32.to_le_bytes()); // src_mip_level
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_array_layer
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_x
        stream.extend_from_slice(&0u32.to_le_bytes()); // dst_y
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_x
        stream.extend_from_slice(&0u32.to_le_bytes()); // src_y
        stream.extend_from_slice(&1u32.to_le_bytes()); // width
        stream.extend_from_slice(&1u32.to_le_bytes()); // height
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[8..12].copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .unwrap();
        exec.poll_wait();

        let pixels = exec.read_texture_rgba8(DST).await.unwrap();
        assert_eq!(pixels, [0, 255, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_set_constant_buffers_binds_cb0_ranges() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        #[repr(C)]
        #[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
        struct VertexPos {
            pos: [f32; 3],
        }

        const VB: u32 = 1;
        const CB: u32 = 2;
        const RT: u32 = 3;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vertices = [
            VertexPos {
                pos: [-1.0, -1.0, 0.0],
            },
            VertexPos {
                pos: [-1.0, 3.0, 0.0],
            },
            VertexPos {
                pos: [3.0, -1.0, 0.0],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);
        let vb_size = vb_bytes.len() as u64;

        let mut guest_mem = VecGuestMemory::new(0x1000);
        let alloc_id = 1u32;
        let alloc_gpa = 0x100u64;
        guest_mem.write(alloc_gpa, vb_bytes).unwrap();

        let allocs = [AerogpuAllocEntry {
            alloc_id,
            flags: 0,
            gpa: alloc_gpa,
            size_bytes: vb_size,
            reserved0: 0,
        }];

        let dxbc_vs = load_fixture("vs_matrix.dxbc");
        let dxbc_ps = build_ps_constant_color_dxbc([0.0, 1.0, 0.0, 1.0]);
        let ilay = build_ilay_pos3_only();

        // Place the 4x4 identity matrix at an unaligned offset inside the constant buffer so the
        // executor has to use a scratch upload to satisfy WebGPU's uniform-buffer alignment rules.
        let mut cb_data = vec![0u8; 16];
        let matrix: [f32; 16] = [
            1.0, 0.0, 0.0, 0.0, //
            0.0, 1.0, 0.0, 0.0, //
            0.0, 0.0, 1.0, 0.0, //
            0.0, 0.0, 0.0, 1.0, //
        ];
        cb_data.extend_from_slice(bytemuck::cast_slice(&matrix));
        let cb_size = cb_data.len() as u64;

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (VB)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_BUFFER);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&vb_size.to_le_bytes());
        stream.extend_from_slice(&alloc_id.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // RESOURCE_DIRTY_RANGE (full VB)
        let start = begin_cmd(&mut stream, OPCODE_RESOURCE_DIRTY_RANGE);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&vb_size.to_le_bytes()); // size_bytes
        end_cmd(&mut stream, start);

        // CREATE_BUFFER (CB)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_BUFFER);
        stream.extend_from_slice(&CB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER.to_le_bytes());
        stream.extend_from_slice(&cb_size.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id (host alloc)
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (CB bytes)
        let start = begin_cmd(&mut stream, OPCODE_UPLOAD_RESOURCE);
        stream.extend_from_slice(&CB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(cb_data.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(&cb_data);
        stream.resize(stream.len() + (align4(cb_data.len()) - cb_data.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_TEXTURE2D);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_FORMAT_R8G8B8A8_UNORM.to_le_bytes());
        stream.extend_from_slice(&4u32.to_le_bytes()); // width
        stream.extend_from_slice(&4u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id (host alloc)
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS
        let start = begin_cmd(&mut stream, OPCODE_SET_RENDER_TARGETS);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // VIEWPORT 0..4
        let start = begin_cmd(&mut stream, OPCODE_SET_VIEWPORT);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        end_cmd(&mut stream, start);

        // SCISSOR 0..4
        let start = begin_cmd(&mut stream, OPCODE_SET_SCISSOR);
        stream.extend_from_slice(&0i32.to_le_bytes());
        stream.extend_from_slice(&0i32.to_le_bytes());
        stream.extend_from_slice(&4i32.to_le_bytes());
        stream.extend_from_slice(&4i32.to_le_bytes());
        end_cmd(&mut stream, start);

        // CLEAR (black)
        let start = begin_cmd(&mut stream, OPCODE_CLEAR);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SHADER_DXBC);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(dxbc_vs.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&dxbc_vs);
        stream.resize(stream.len() + (align4(dxbc_vs.len()) - dxbc_vs.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_SHADER_DXBC);
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // stage = pixel
        stream.extend_from_slice(&(dxbc_ps.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&dxbc_ps);
        stream.resize(stream.len() + (align4(dxbc_ps.len()) - dxbc_ps.len()), 0);
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, OPCODE_BIND_SHADERS);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_INPUT_LAYOUT (ILAY blob)
        let start = begin_cmd(&mut stream, OPCODE_CREATE_INPUT_LAYOUT);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ilay.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&ilay);
        stream.resize(stream.len() + (align4(ilay.len()) - ilay.len()), 0);
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, OPCODE_SET_INPUT_LAYOUT);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_CONSTANT_BUFFERS (VS cb0 = CB[16..80])
        let start = begin_cmd(&mut stream, OPCODE_SET_CONSTANT_BUFFERS);
        stream.extend_from_slice(&0u32.to_le_bytes()); // shader_stage = vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&CB.to_le_bytes());
        stream.extend_from_slice(&16u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&64u32.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS
        let start = begin_cmd(&mut stream, OPCODE_SET_VERTEX_BUFFERS);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&(std::mem::size_of::<VertexPos>() as u32).to_le_bytes()); // stride
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, OPCODE_SET_PRIMITIVE_TOPOLOGY);
        stream.extend_from_slice(&AEROGPU_TOPOLOGY_TRIANGLELIST.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // DRAW
        let start = begin_cmd(&mut stream, OPCODE_DRAW);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // PRESENT
        let start = begin_cmd(&mut stream, OPCODE_PRESENT);
        stream.extend_from_slice(&0u32.to_le_bytes()); // scanout_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[8..12].copy_from_slice(&total_size.to_le_bytes());

        exec.execute_cmd_stream(&stream, Some(&allocs), &mut guest_mem)
            .unwrap();
        exec.poll_wait();

        let pixels = exec.read_texture_rgba8(RT).await.unwrap();
        assert_eq!(pixels.len(), 4 * 4 * 4);
        for px in pixels.chunks_exact(4) {
            assert_eq!(px, &[0, 255, 0, 255]);
        }
    });
}
