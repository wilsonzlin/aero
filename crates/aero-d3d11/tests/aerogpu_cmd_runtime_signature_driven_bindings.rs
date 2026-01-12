mod common;

use aero_d3d11::input_layout::fnv1a_32;
use aero_d3d11::runtime::aerogpu_execute::AerogpuCmdRuntime;
use aero_d3d11::runtime::aerogpu_state::{PrimitiveTopology, RasterizerState, VertexBufferBinding};

const DXBC_VS_MATRIX: &[u8] = include_bytes!("fixtures/vs_matrix.dxbc");
const DXBC_VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");
const DXBC_VS_PASSTHROUGH_TEXCOORD: &[u8] = include_bytes!("fixtures/vs_passthrough_texcoord.dxbc");
const DXBC_PS_SAMPLE: &[u8] = include_bytes!("fixtures/ps_sample.dxbc");
const ILAY_POS3_COLOR: &[u8] = include_bytes!("fixtures/ilay_pos3_color.bin");
const ILAY_POS3_TEX2: &[u8] = include_bytes!("fixtures/ilay_pos3_tex2.bin");

fn align4(len: usize) -> usize {
    (len + 3) & !3
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

fn build_signature_chunk_v1(params: &[SigParam]) -> Vec<u8> {
    // Mirrors the 32-byte entry layout used by `ISG1`/`OSG1` signature chunks.
    let mut out = Vec::new();
    out.extend_from_slice(&(params.len() as u32).to_le_bytes()); // param_count
    out.extend_from_slice(&8u32.to_le_bytes()); // param_offset

    let entry_size = 32usize;
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
        // out[base + 22..base + 24] reserved/padding
        out[base + 24..base + 28].copy_from_slice(&0u32.to_le_bytes()); // stream
        out[base + 28..base + 32].copy_from_slice(&0u32.to_le_bytes()); // min_precision
    }

    out
}

fn build_ps_solid_red_dxbc() -> Vec<u8> {
    // Hand-authored minimal DXBC container: empty ISGN + OSGN(SV_Target0) + SHDR(token stream).
    //
    // Token stream (SM4 subset):
    //   mov o0, l(1, 0, 0, 1)
    //   ret
    let isgn = build_signature_chunk(&[]);
    let osgn = build_signature_chunk(&[SigParam {
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

    let red = 1.0f32.to_bits();
    let zero = 0.0f32.to_bits();
    let one = 1.0f32.to_bits();

    let mut tokens = vec![
        version_token,
        0, // length patched below
        mov_token,
        dst_o0,
        0, // o0 index
        imm_vec4,
        red,
        zero,
        zero,
        one,
        ret_token,
    ];
    tokens[1] = tokens.len() as u32;

    let mut shdr = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        shdr.extend_from_slice(&t.to_le_bytes());
    }

    build_dxbc(&[(*b"ISGN", isgn), (*b"OSGN", osgn), (*b"SHDR", shdr)])
}

fn build_ps_cbuffer0_dxbc() -> Vec<u8> {
    // Hand-authored minimal DXBC container: ISGN(SV_Position + COLOR0) + OSGN(SV_Target0) +
    // SHDR(token stream).
    //
    // Token stream (SM4 subset):
    //   mov o0, cb0[0]
    //   ret
    //
    // The COLOR0 input is unused, but included to satisfy WebGPU stage-interface validation for the
    // `vs_passthrough.dxbc` fixture which outputs `@location(1)`.
    let isgn = build_signature_chunk(&[
        SigParam {
            semantic_name: "SV_Position",
            semantic_index: 0,
            register: 0,
            mask: 0x0f,
        },
        SigParam {
            semantic_name: "COLOR",
            semantic_index: 0,
            register: 1,
            mask: 0x0f,
        },
    ]);
    let osgn = build_signature_chunk(&[SigParam {
        semantic_name: "SV_Target",
        semantic_index: 0,
        register: 0,
        mask: 0x0f,
    }]);

    // ps_4_0
    let version_token = 0x40u32;

    // mov o0, cb0[0]
    let mov_token = 0x01u32 | (6u32 << 11);
    let dst_o0 = 0x0010_f022u32;
    // Constant-buffer operand (slot + register index).
    // - 4-component operand (`num_components = 2`)
    // - component selection mode = swizzle
    // - 2D immediate indices: [slot, reg]
    // - swizzle = XYZW (0xE4)
    let cb0_reg0 = 0x002e_4086u32;

    let ret_token = 0x3eu32 | (1u32 << 11);

    let mut tokens = vec![
        version_token,
        0, // length patched below
        mov_token,
        dst_o0,
        0, // o0 index
        cb0_reg0,
        0, // cb slot
        0, // cb reg
        ret_token,
    ];
    tokens[1] = tokens.len() as u32;

    let mut shdr = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        shdr.extend_from_slice(&t.to_le_bytes());
    }

    build_dxbc(&[(*b"ISGN", isgn), (*b"OSGN", osgn), (*b"SHDR", shdr)])
}

fn build_ps_cbuffer0_sm5_dxbc() -> Vec<u8> {
    // Same as `build_ps_cbuffer0_dxbc`, but encoded as SM5 (`SHEX`, ps_5_0).
    let isgn = build_signature_chunk(&[
        SigParam {
            semantic_name: "SV_Position",
            semantic_index: 0,
            register: 0,
            mask: 0x0f,
        },
        SigParam {
            semantic_name: "COLOR",
            semantic_index: 0,
            register: 1,
            mask: 0x0f,
        },
    ]);
    let osgn = build_signature_chunk(&[SigParam {
        semantic_name: "SV_Target",
        semantic_index: 0,
        register: 0,
        mask: 0x0f,
    }]);

    // ps_5_0
    let version_token = 0x50u32;

    // mov o0, cb0[0]
    let mov_token = 0x01u32 | (6u32 << 11);
    let dst_o0 = 0x0010_f022u32;
    let cb0_reg0 = 0x002e_4086u32;
    let ret_token = 0x3eu32 | (1u32 << 11);

    let mut tokens = vec![
        version_token,
        0, // length patched below
        mov_token,
        dst_o0,
        0, // o0 index
        cb0_reg0,
        0, // cb slot
        0, // cb reg
        ret_token,
    ];
    tokens[1] = tokens.len() as u32;

    let mut shex = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        shex.extend_from_slice(&t.to_le_bytes());
    }

    build_dxbc(&[(*b"ISGN", isgn), (*b"OSGN", osgn), (*b"SHEX", shex)])
}

fn build_vs_passthrough_pos_sm5_dxbc() -> Vec<u8> {
    // Minimal VS (vs_5_0) that passes POSITION0 (v0.xyz) through to SV_Position (o0).
    //
    // Token stream:
    //   mov o0, v0
    //   ret
    let isgn = build_signature_chunk(&[SigParam {
        semantic_name: "POSITION",
        semantic_index: 0,
        register: 0,
        mask: 0x07,
    }]);
    let osgn = build_signature_chunk(&[SigParam {
        semantic_name: "SV_Position",
        semantic_index: 0,
        register: 0,
        mask: 0x0f,
    }]);

    // vs_5_0: stage type 1, major 5, minor 0.
    let version_token = 0x0001_0050u32;

    // mov o0, v0
    let mov_token = 0x01u32 | (5u32 << 11);
    let dst_o0 = 0x0010_f022u32;
    let src_v0 = 0x001e_4016u32;
    let ret_token = 0x3eu32 | (1u32 << 11);

    let mut tokens = vec![
        version_token,
        0, // length patched below
        mov_token,
        dst_o0,
        0, // o0 index
        src_v0,
        0, // v0 index
        ret_token,
    ];
    tokens[1] = tokens.len() as u32;

    let mut shex = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        shex.extend_from_slice(&t.to_le_bytes());
    }

    build_dxbc(&[(*b"ISGN", isgn), (*b"OSGN", osgn), (*b"SHEX", shex)])
}

fn build_ps_sample_l_t0_s0_sm5_dxbc(u: f32, v: f32) -> Vec<u8> {
    // Minimal PS (ps_5_0) that samples `t0`/`s0` at a constant coordinate and returns it as
    // SV_Target0.
    //
    // Token stream:
    //   sample_l o0, l(u, v, 0, 0), t0, s0, l(0)
    //   ret
    let isgn = build_signature_chunk(&[]);
    let osgn = build_signature_chunk(&[SigParam {
        semantic_name: "SV_Target",
        semantic_index: 0,
        register: 0,
        mask: 0x0f,
    }]);

    // ps_5_0
    let version_token = 0x50u32;

    let sample_l_opcode_token = 0x46u32 | (14u32 << 11);
    let ret_token = 0x3eu32 | (1u32 << 11);

    let dst_o0 = 0x0010_f022u32;
    let imm_vec4 = 0x0000_f042u32;
    let imm_scalar = 0x0000_0049u32;
    let t0 = 0x0010_0072u32;
    let s0 = 0x0010_0062u32;

    let u = u.to_bits();
    let v = v.to_bits();

    let mut tokens = vec![
        version_token,
        0, // length patched below
        // sample_l o0, l(u,v,0,0), t0, s0, l(0)
        sample_l_opcode_token,
        dst_o0,
        0, // o0 index
        imm_vec4,
        u,
        v,
        0,
        0,
        t0,
        0,
        s0,
        0,
        imm_scalar,
        0, // lod=0
        ret_token,
    ];
    tokens[1] = tokens.len() as u32;

    let mut shex = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        shex.extend_from_slice(&t.to_le_bytes());
    }

    build_dxbc(&[(*b"ISGN", isgn), (*b"OSGN", osgn), (*b"SHEX", shex)])
}

fn build_ps_cbuffer0_sm5_sig_v1_dxbc() -> Vec<u8> {
    // Equivalent to `build_ps_cbuffer0_sm5_dxbc`, but uses `ISG1`/`OSG1` signature chunks with the
    // 32-byte v1 entry layout.
    let isgn = build_signature_chunk_v1(&[
        SigParam {
            semantic_name: "SV_Position",
            semantic_index: 0,
            register: 0,
            mask: 0x0f,
        },
        SigParam {
            semantic_name: "COLOR",
            semantic_index: 0,
            register: 1,
            mask: 0x0f,
        },
    ]);
    let osgn = build_signature_chunk_v1(&[SigParam {
        semantic_name: "SV_Target",
        semantic_index: 0,
        register: 0,
        mask: 0x0f,
    }]);

    // ps_5_0
    let version_token = 0x50u32;

    // mov o0, cb0[0]
    let mov_token = 0x01u32 | (6u32 << 11);
    let dst_o0 = 0x0010_f022u32;
    let cb0_reg0 = 0x002e_4086u32;
    let ret_token = 0x3eu32 | (1u32 << 11);

    let mut tokens = vec![
        version_token,
        0, // length patched below
        mov_token,
        dst_o0,
        0, // o0 index
        cb0_reg0,
        0, // cb slot
        0, // cb reg
        ret_token,
    ];
    tokens[1] = tokens.len() as u32;

    let mut shex = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        shex.extend_from_slice(&t.to_le_bytes());
    }

    build_dxbc(&[(*b"ISG1", isgn), (*b"OSG1", osgn), (*b"SHEX", shex)])
}

fn build_ps_passthrough_color_dxbc() -> Vec<u8> {
    // Minimal PS that returns COLOR0 (`v1`) as SV_Target0.
    let isgn = build_signature_chunk(&[
        SigParam {
            semantic_name: "SV_Position",
            semantic_index: 0,
            register: 0,
            mask: 0x0f,
        },
        SigParam {
            semantic_name: "COLOR",
            semantic_index: 0,
            register: 1,
            mask: 0x0f,
        },
    ]);
    let osgn = build_signature_chunk(&[SigParam {
        semantic_name: "SV_Target",
        semantic_index: 0,
        register: 0,
        mask: 0x0f,
    }]);

    // ps_4_0
    let version_token = 0x40u32;
    // mov o0, v1
    let mov_token = 0x01u32 | (5u32 << 11);
    let dst_o0 = 0x0010_f022u32;
    let src_v1 = 0x001e_4016u32;
    let ret_token = 0x3eu32 | (1u32 << 11);

    let mut tokens = vec![
        version_token,
        0, // length patched below
        mov_token,
        dst_o0,
        0, // o0 index
        src_v1,
        1, // v1 index
        ret_token,
    ];
    tokens[1] = tokens.len() as u32;

    let mut shdr = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        shdr.extend_from_slice(&t.to_le_bytes());
    }

    build_dxbc(&[(*b"ISGN", isgn), (*b"OSGN", osgn), (*b"SHDR", shdr)])
}

fn build_vs_sample_t0_s0_to_color1_dxbc(u: f32, v: f32) -> Vec<u8> {
    // Minimal VS that samples t0+s0 at a constant coord and outputs it as COLOR0 (o1). Position is
    // passed through from input POSITION0 (v0) to SV_Position (o0).
    //
    // Token stream (SM4 subset):
    //   sample_l o1, l(u, v, 0, 0), t0, s0, l(0)
    //   mov o0, v0
    //   ret
    let isgn = build_signature_chunk(&[SigParam {
        semantic_name: "POSITION",
        semantic_index: 0,
        register: 0,
        mask: 0x07,
    }]);
    let osgn = build_signature_chunk(&[
        SigParam {
            semantic_name: "SV_Position",
            semantic_index: 0,
            register: 0,
            mask: 0x0f,
        },
        SigParam {
            semantic_name: "COLOR",
            semantic_index: 0,
            register: 1,
            mask: 0x0f,
        },
    ]);

    // vs_4_0
    let version_token = 0x0001_0040u32;

    // Vertex shaders cannot use implicit-derivative sampling (`textureSample`) in WebGPU/WGSL, so
    // use the `sample_l` variant which translates to `textureSampleLevel` (explicit LOD) and is
    // valid in the vertex stage.
    let sample_l_opcode_token = 0x46u32 | (14u32 << 11);
    let mov_token = 0x01u32 | (5u32 << 11);
    let ret_token = 0x3eu32 | (1u32 << 11);

    let dst_o = 0x0010_f022u32;
    let imm_vec4 = 0x0000_f042u32;
    let imm_scalar = 0x0000_0049u32;
    let t0 = 0x0010_0072u32;
    let s0 = 0x0010_0062u32;
    let src_v0 = 0x001e_4016u32;

    let u = u.to_bits();
    let v = v.to_bits();

    let mut tokens = vec![
        version_token,
        0, // length patched below
        // sample_l o1, l(u,v,0,0), t0, s0, l(0)
        sample_l_opcode_token,
        dst_o,
        1, // o1 index
        imm_vec4,
        u,
        v,
        0,
        0,
        t0,
        0,
        s0,
        0,
        imm_scalar,
        0, // lod=0
        // mov o0, v0
        mov_token,
        dst_o,
        0, // o0
        src_v0,
        0, // v0
        ret_token,
    ];
    tokens[1] = tokens.len() as u32;

    let mut shdr = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        shdr.extend_from_slice(&t.to_le_bytes());
    }

    build_dxbc(&[(*b"ISGN", isgn), (*b"OSGN", osgn), (*b"SHDR", shdr)])
}

fn build_vs_sample_t0_s0_to_color1_sm5_dxbc(u: f32, v: f32) -> Vec<u8> {
    // SM5 variant of `build_vs_sample_t0_s0_to_color1_dxbc`: same token stream, but encoded as
    // `vs_5_0` in a `SHEX` chunk.
    let isgn = build_signature_chunk(&[SigParam {
        semantic_name: "POSITION",
        semantic_index: 0,
        register: 0,
        mask: 0x07,
    }]);
    let osgn = build_signature_chunk(&[
        SigParam {
            semantic_name: "SV_Position",
            semantic_index: 0,
            register: 0,
            mask: 0x0f,
        },
        SigParam {
            semantic_name: "COLOR",
            semantic_index: 0,
            register: 1,
            mask: 0x0f,
        },
    ]);

    // vs_5_0
    let version_token = 0x0001_0050u32;

    let sample_l_opcode_token = 0x46u32 | (14u32 << 11);
    let mov_token = 0x01u32 | (5u32 << 11);
    let ret_token = 0x3eu32 | (1u32 << 11);

    let dst_o = 0x0010_f022u32;
    let imm_vec4 = 0x0000_f042u32;
    let imm_scalar = 0x0000_0049u32;
    let t0 = 0x0010_0072u32;
    let s0 = 0x0010_0062u32;
    let src_v0 = 0x001e_4016u32;

    let u = u.to_bits();
    let v = v.to_bits();

    let mut tokens = vec![
        version_token,
        0, // length patched below
        // sample_l o1, l(u,v,0,0), t0, s0, l(0)
        sample_l_opcode_token,
        dst_o,
        1, // o1 index
        imm_vec4,
        u,
        v,
        0,
        0,
        t0,
        0,
        s0,
        0,
        imm_scalar,
        0, // lod=0
        // mov o0, v0
        mov_token,
        dst_o,
        0, // o0
        src_v0,
        0, // v0
        ret_token,
    ];
    tokens[1] = tokens.len() as u32;

    let mut shex = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        shex.extend_from_slice(&t.to_le_bytes());
    }

    build_dxbc(&[(*b"ISGN", isgn), (*b"OSGN", osgn), (*b"SHEX", shex)])
}

fn build_ilay_pos3() -> Vec<u8> {
    // Build an ILAY blob that matches the `vs_matrix.dxbc` fixture input signature: POSITION0 only.
    //
    // struct aerogpu_input_layout_blob_header (16 bytes)
    // struct aerogpu_input_layout_element_dxgi (28 bytes)
    let mut blob = Vec::new();
    blob.extend_from_slice(&0x5941_4C49u32.to_le_bytes()); // "ILAY"
    blob.extend_from_slice(&1u32.to_le_bytes()); // version
    blob.extend_from_slice(&1u32.to_le_bytes()); // element_count
    blob.extend_from_slice(&0u32.to_le_bytes()); // reserved0

    // POSITION0: R32G32B32_FLOAT @ slot0 offset0
    blob.extend_from_slice(&fnv1a_32(b"POSITION").to_le_bytes());
    blob.extend_from_slice(&0u32.to_le_bytes()); // semantic_index
    blob.extend_from_slice(&6u32.to_le_bytes()); // DXGI_FORMAT_R32G32B32_FLOAT
    blob.extend_from_slice(&0u32.to_le_bytes()); // input_slot
    blob.extend_from_slice(&0u32.to_le_bytes()); // aligned_byte_offset
    blob.extend_from_slice(&0u32.to_le_bytes()); // input_slot_class (per-vertex)
    blob.extend_from_slice(&0u32.to_le_bytes()); // instance_data_step_rate

    blob
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VertexPos3 {
    pos: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VertexPos3Tex2 {
    pos: [f32; 3],
    tex: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VertexPos3Color4 {
    pos: [f32; 3],
    color: [f32; 4],
}

#[test]
fn aerogpu_cmd_runtime_signature_driven_constant_buffer_binding() {
    pollster::block_on(async {
        let mut rt = match AerogpuCmdRuntime::new_for_tests().await {
            Ok(rt) => rt,
            Err(err) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };

        const VS: u32 = 1;
        const PS: u32 = 2;
        const IL: u32 = 3;
        const VB: u32 = 4;
        const CB0: u32 = 5;
        const RTEX: u32 = 6;

        rt.create_shader_dxbc(VS, DXBC_VS_MATRIX).unwrap();
        rt.create_shader_dxbc(PS, &build_ps_solid_red_dxbc()).unwrap();
        rt.create_input_layout(IL, &build_ilay_pos3()).unwrap();

        let vertices: [VertexPos3; 3] = [
            VertexPos3 {
                pos: [-1.0, -1.0, 0.0],
            },
            VertexPos3 {
                pos: [3.0, -1.0, 0.0],
            },
            VertexPos3 {
                pos: [-1.0, 3.0, 0.0],
            },
        ];
        rt.create_buffer(
            VB,
            std::mem::size_of_val(&vertices) as u64,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(VB, 0, bytemuck::bytes_of(&vertices))
            .unwrap();

        let identity: [[f32; 4]; 4] = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        rt.create_buffer(
            CB0,
            std::mem::size_of_val(&identity) as u64,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(CB0, 0, bytemuck::bytes_of(&identity))
            .unwrap();
        rt.set_vs_constant_buffer(0, Some(CB0));

        rt.create_texture2d(
            RTEX,
            4,
            4,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );

        let mut colors = [None; 8];
        colors[0] = Some(RTEX);
        rt.set_render_targets(&colors, None);

        rt.bind_shaders(Some(VS), Some(PS));
        rt.set_input_layout(Some(IL));
        rt.set_vertex_buffers(
            0,
            &[VertexBufferBinding {
                buffer: VB,
                stride: std::mem::size_of::<VertexPos3>() as u32,
                offset: 0,
            }],
        );
        rt.set_primitive_topology(PrimitiveTopology::TriangleList);
        rt.set_rasterizer_state(RasterizerState {
            cull_mode: None,
            front_face: wgpu::FrontFace::Ccw,
            scissor_enable: false,
        });

        rt.draw(3, 1, 0, 0).unwrap();
        rt.poll_wait();

        let pixels = rt.read_texture_rgba8(RTEX).await.unwrap();
        assert_eq!(pixels.len(), 4 * 4 * 4);
        for (i, px) in pixels.chunks_exact(4).enumerate() {
            assert_eq!(px, &[255, 0, 0, 255], "pixel index {i}");
        }
    });
}

#[test]
fn aerogpu_cmd_runtime_signature_driven_ps_constant_buffer_binding() {
    pollster::block_on(async {
        let mut rt = match AerogpuCmdRuntime::new_for_tests().await {
            Ok(rt) => rt,
            Err(err) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };

        const VS: u32 = 1;
        const PS: u32 = 2;
        const IL: u32 = 3;
        const VB: u32 = 4;
        const CB0: u32 = 5;
        const RTEX: u32 = 6;

        rt.create_shader_dxbc(VS, DXBC_VS_PASSTHROUGH).unwrap();
        rt.create_shader_dxbc(PS, &build_ps_cbuffer0_dxbc())
            .unwrap();
        rt.create_input_layout(IL, ILAY_POS3_COLOR).unwrap();

        let vertices: [VertexPos3Color4; 3] = [
            VertexPos3Color4 {
                pos: [-1.0, -1.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            VertexPos3Color4 {
                pos: [3.0, -1.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            VertexPos3Color4 {
                pos: [-1.0, 3.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
        ];
        rt.create_buffer(
            VB,
            std::mem::size_of_val(&vertices) as u64,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(VB, 0, bytemuck::bytes_of(&vertices))
            .unwrap();

        // cb0[0] = (0, 0, 1, 1) => solid blue
        let cb0_color: [f32; 4] = [0.0, 0.0, 1.0, 1.0];
        rt.create_buffer(
            CB0,
            std::mem::size_of_val(&cb0_color) as u64,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(CB0, 0, bytemuck::bytes_of(&cb0_color))
            .unwrap();
        rt.set_ps_constant_buffer(0, Some(CB0));

        rt.create_texture2d(
            RTEX,
            4,
            4,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );

        let mut colors = [None; 8];
        colors[0] = Some(RTEX);
        rt.set_render_targets(&colors, None);

        rt.bind_shaders(Some(VS), Some(PS));
        rt.set_input_layout(Some(IL));
        rt.set_vertex_buffers(
            0,
            &[VertexBufferBinding {
                buffer: VB,
                stride: std::mem::size_of::<VertexPos3Color4>() as u32,
                offset: 0,
            }],
        );
        rt.set_primitive_topology(PrimitiveTopology::TriangleList);
        rt.set_rasterizer_state(RasterizerState {
            cull_mode: None,
            front_face: wgpu::FrontFace::Ccw,
            scissor_enable: false,
        });

        rt.draw(3, 1, 0, 0).unwrap();
        rt.poll_wait();

        let pixels = rt.read_texture_rgba8(RTEX).await.unwrap();
        assert_eq!(pixels.len(), 4 * 4 * 4);
        for (i, px) in pixels.chunks_exact(4).enumerate() {
            assert_eq!(px, &[0, 0, 255, 255], "pixel index {i}");
        }
    });
}

#[test]
fn aerogpu_cmd_runtime_signature_driven_ps_constant_buffer_binding_sm5() {
    pollster::block_on(async {
        let mut rt = match AerogpuCmdRuntime::new_for_tests().await {
            Ok(rt) => rt,
            Err(err) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };

        const VS: u32 = 1;
        const PS: u32 = 2;
        const IL: u32 = 3;
        const VB: u32 = 4;
        const CB0: u32 = 5;
        const RTEX: u32 = 6;

        rt.create_shader_dxbc(VS, DXBC_VS_PASSTHROUGH).unwrap();
        rt.create_shader_dxbc(PS, &build_ps_cbuffer0_sm5_dxbc())
            .unwrap();
        rt.create_input_layout(IL, ILAY_POS3_COLOR).unwrap();

        let vertices: [VertexPos3Color4; 3] = [
            VertexPos3Color4 {
                pos: [-1.0, -1.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            VertexPos3Color4 {
                pos: [3.0, -1.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            VertexPos3Color4 {
                pos: [-1.0, 3.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
        ];
        rt.create_buffer(
            VB,
            std::mem::size_of_val(&vertices) as u64,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(VB, 0, bytemuck::bytes_of(&vertices))
            .unwrap();

        // cb0[0] = (0, 0, 1, 1) => solid blue
        let cb0_color: [f32; 4] = [0.0, 0.0, 1.0, 1.0];
        rt.create_buffer(
            CB0,
            std::mem::size_of_val(&cb0_color) as u64,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(CB0, 0, bytemuck::bytes_of(&cb0_color))
            .unwrap();
        rt.set_ps_constant_buffer(0, Some(CB0));

        rt.create_texture2d(
            RTEX,
            4,
            4,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );

        let mut colors = [None; 8];
        colors[0] = Some(RTEX);
        rt.set_render_targets(&colors, None);

        rt.bind_shaders(Some(VS), Some(PS));
        rt.set_input_layout(Some(IL));
        rt.set_vertex_buffers(
            0,
            &[VertexBufferBinding {
                buffer: VB,
                stride: std::mem::size_of::<VertexPos3Color4>() as u32,
                offset: 0,
            }],
        );
        rt.set_primitive_topology(PrimitiveTopology::TriangleList);
        rt.set_rasterizer_state(RasterizerState {
            cull_mode: None,
            front_face: wgpu::FrontFace::Ccw,
            scissor_enable: false,
        });

        rt.draw(3, 1, 0, 0).unwrap();
        rt.poll_wait();

        let pixels = rt.read_texture_rgba8(RTEX).await.unwrap();
        assert_eq!(pixels.len(), 4 * 4 * 4);
        for (i, px) in pixels.chunks_exact(4).enumerate() {
            assert_eq!(px, &[0, 0, 255, 255], "pixel index {i}");
        }
    });
}

#[test]
fn aerogpu_cmd_runtime_signature_driven_ps_constant_buffer_binding_sm5_sig_v1() {
    pollster::block_on(async {
        let mut rt = match AerogpuCmdRuntime::new_for_tests().await {
            Ok(rt) => rt,
            Err(err) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };

        const VS: u32 = 1;
        const PS: u32 = 2;
        const IL: u32 = 3;
        const VB: u32 = 4;
        const CB0: u32 = 5;
        const RTEX: u32 = 6;

        rt.create_shader_dxbc(VS, DXBC_VS_PASSTHROUGH).unwrap();
        rt.create_shader_dxbc(PS, &build_ps_cbuffer0_sm5_sig_v1_dxbc())
            .unwrap();
        rt.create_input_layout(IL, ILAY_POS3_COLOR).unwrap();

        let vertices: [VertexPos3Color4; 3] = [
            VertexPos3Color4 {
                pos: [-1.0, -1.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            VertexPos3Color4 {
                pos: [3.0, -1.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            VertexPos3Color4 {
                pos: [-1.0, 3.0, 0.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
        ];
        rt.create_buffer(
            VB,
            std::mem::size_of_val(&vertices) as u64,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(VB, 0, bytemuck::bytes_of(&vertices))
            .unwrap();

        // cb0[0] = (0, 0, 1, 1) => solid blue
        let cb0_color: [f32; 4] = [0.0, 0.0, 1.0, 1.0];
        rt.create_buffer(
            CB0,
            std::mem::size_of_val(&cb0_color) as u64,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(CB0, 0, bytemuck::bytes_of(&cb0_color))
            .unwrap();
        rt.set_ps_constant_buffer(0, Some(CB0));

        rt.create_texture2d(
            RTEX,
            4,
            4,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );

        let mut colors = [None; 8];
        colors[0] = Some(RTEX);
        rt.set_render_targets(&colors, None);

        rt.bind_shaders(Some(VS), Some(PS));
        rt.set_input_layout(Some(IL));
        rt.set_vertex_buffers(
            0,
            &[VertexBufferBinding {
                buffer: VB,
                stride: std::mem::size_of::<VertexPos3Color4>() as u32,
                offset: 0,
            }],
        );
        rt.set_primitive_topology(PrimitiveTopology::TriangleList);
        rt.set_rasterizer_state(RasterizerState {
            cull_mode: None,
            front_face: wgpu::FrontFace::Ccw,
            scissor_enable: false,
        });

        rt.draw(3, 1, 0, 0).unwrap();
        rt.poll_wait();

        let pixels = rt.read_texture_rgba8(RTEX).await.unwrap();
        assert_eq!(pixels.len(), 4 * 4 * 4);
        for (i, px) in pixels.chunks_exact(4).enumerate() {
            assert_eq!(px, &[0, 0, 255, 255], "pixel index {i}");
        }
    });
}

#[test]
fn aerogpu_cmd_runtime_signature_driven_texture_sampler_binding_sm5() {
    pollster::block_on(async {
        let mut rt = match AerogpuCmdRuntime::new_for_tests().await {
            Ok(rt) => rt,
            Err(err) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };

        const VS: u32 = 1;
        const PS: u32 = 2;
        const IL: u32 = 3;
        const VB: u32 = 4;
        const TEX: u32 = 5;
        const RTEX: u32 = 6;

        rt.create_shader_dxbc(VS, &build_vs_passthrough_pos_sm5_dxbc())
            .unwrap();
        rt.create_shader_dxbc(PS, &build_ps_sample_l_t0_s0_sm5_dxbc(0.0, 0.0))
            .unwrap();
        rt.create_input_layout(IL, &build_ilay_pos3()).unwrap();

        let vertices: [VertexPos3; 3] = [
            VertexPos3 {
                pos: [-1.0, -1.0, 0.0],
            },
            VertexPos3 {
                pos: [3.0, -1.0, 0.0],
            },
            VertexPos3 {
                pos: [-1.0, 3.0, 0.0],
            },
        ];
        rt.create_buffer(
            VB,
            std::mem::size_of_val(&vertices) as u64,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(VB, 0, bytemuck::bytes_of(&vertices))
            .unwrap();

        rt.create_texture2d(
            TEX,
            2,
            2,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        );
        let green_px: [u8; 4] = [0, 255, 0, 255];
        let tex_data = [
            green_px, green_px, //
            green_px, green_px, //
        ];
        rt.write_texture_rgba8(TEX, 2, 2, 2 * 4, bytemuck::bytes_of(&tex_data))
            .unwrap();
        rt.set_ps_texture(0, Some(TEX));

        rt.create_texture2d(
            RTEX,
            1,
            1,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );

        let mut colors = [None; 8];
        colors[0] = Some(RTEX);
        rt.set_render_targets(&colors, None);

        rt.bind_shaders(Some(VS), Some(PS));
        rt.set_input_layout(Some(IL));
        rt.set_vertex_buffers(
            0,
            &[VertexBufferBinding {
                buffer: VB,
                stride: std::mem::size_of::<VertexPos3>() as u32,
                offset: 0,
            }],
        );
        rt.set_primitive_topology(PrimitiveTopology::TriangleList);
        rt.set_rasterizer_state(RasterizerState {
            cull_mode: None,
            front_face: wgpu::FrontFace::Ccw,
            scissor_enable: false,
        });

        rt.draw(3, 1, 0, 0).unwrap();
        rt.poll_wait();

        let pixels = rt.read_texture_rgba8(RTEX).await.unwrap();
        assert_eq!(pixels, vec![0, 255, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_runtime_signature_driven_vs_texture_sampler_binding() {
    pollster::block_on(async {
        let mut rt = match AerogpuCmdRuntime::new_for_tests().await {
            Ok(rt) => rt,
            Err(err) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };

        const VS: u32 = 1;
        const PS: u32 = 2;
        const IL: u32 = 3;
        const VB: u32 = 4;
        const TEX: u32 = 5;
        const RTEX: u32 = 6;
        const SAMPLER: u32 = 7;

        rt.create_shader_dxbc(VS, &build_vs_sample_t0_s0_to_color1_dxbc(1.1, 0.5))
            .unwrap();
        rt.create_shader_dxbc(PS, &build_ps_passthrough_color_dxbc())
            .unwrap();
        rt.create_input_layout(IL, &build_ilay_pos3()).unwrap();

        let vertices: [VertexPos3; 3] = [
            VertexPos3 {
                pos: [-1.0, -1.0, 0.0],
            },
            VertexPos3 {
                pos: [3.0, -1.0, 0.0],
            },
            VertexPos3 {
                pos: [-1.0, 3.0, 0.0],
            },
        ];
        rt.create_buffer(
            VB,
            std::mem::size_of_val(&vertices) as u64,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(VB, 0, bytemuck::bytes_of(&vertices))
            .unwrap();

        // 2x2 texture with left column red and right column green (both rows identical). This
        // makes clamp vs repeat observable for u=1.1.
        rt.create_texture2d(
            TEX,
            2,
            2,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        );
        let tex_data: [[u8; 4]; 4] = [
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [255, 0, 0, 255],
            [0, 255, 0, 255],
        ];
        rt.write_texture_rgba8(TEX, 2, 2, 2 * 4, bytemuck::bytes_of(&tex_data))
            .unwrap();
        rt.set_vs_texture(0, Some(TEX));

        rt.create_texture2d(
            RTEX,
            1,
            1,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );

        let mut colors = [None; 8];
        colors[0] = Some(RTEX);
        rt.set_render_targets(&colors, None);

        rt.bind_shaders(Some(VS), Some(PS));
        rt.set_input_layout(Some(IL));
        rt.set_vertex_buffers(
            0,
            &[VertexBufferBinding {
                buffer: VB,
                stride: std::mem::size_of::<VertexPos3>() as u32,
                offset: 0,
            }],
        );
        rt.set_primitive_topology(PrimitiveTopology::TriangleList);
        rt.set_rasterizer_state(RasterizerState {
            cull_mode: None,
            front_face: wgpu::FrontFace::Ccw,
            scissor_enable: false,
        });

        // Default sampler fallback is clamp-to-edge; u=1.1 should clamp and hit the right column.
        rt.draw(3, 1, 0, 0).unwrap();
        rt.poll_wait();
        let pixels = rt.read_texture_rgba8(RTEX).await.unwrap();
        assert_eq!(pixels, vec![0, 255, 0, 255], "default clamp sampler");

        // Explicit repeat sampler should wrap u=1.1 -> 0.1 and hit the left column.
        rt.create_sampler(
            SAMPLER,
            &wgpu::SamplerDescriptor {
                label: Some("aerogpu_cmd_runtime vs repeat sampler"),
                address_mode_u: wgpu::AddressMode::Repeat,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                address_mode_w: wgpu::AddressMode::ClampToEdge,
                mag_filter: wgpu::FilterMode::Nearest,
                min_filter: wgpu::FilterMode::Nearest,
                mipmap_filter: wgpu::FilterMode::Nearest,
                ..Default::default()
            },
        )
        .unwrap();
        rt.set_vs_sampler(0, Some(SAMPLER));
        rt.draw(3, 1, 0, 0).unwrap();
        rt.poll_wait();
        let pixels = rt.read_texture_rgba8(RTEX).await.unwrap();
        assert_eq!(pixels, vec![255, 0, 0, 255], "repeat sampler");

        // Unbound SRVs should fall back to a dummy (0,0,0,1) texture.
        rt.set_vs_texture(0, None);
        rt.draw(3, 1, 0, 0).unwrap();
        rt.poll_wait();
        let pixels = rt.read_texture_rgba8(RTEX).await.unwrap();
        assert_eq!(pixels, vec![0, 0, 0, 255], "unbound texture fallback");
    });
}

#[test]
fn aerogpu_cmd_runtime_signature_driven_vs_texture_sampler_binding_sm5() {
    pollster::block_on(async {
        let mut rt = match AerogpuCmdRuntime::new_for_tests().await {
            Ok(rt) => rt,
            Err(err) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };

        const VS: u32 = 1;
        const PS: u32 = 2;
        const IL: u32 = 3;
        const VB: u32 = 4;
        const TEX: u32 = 5;
        const RTEX: u32 = 6;
        const SAMPLER: u32 = 7;

        rt.create_shader_dxbc(VS, &build_vs_sample_t0_s0_to_color1_sm5_dxbc(1.1, 0.5))
            .unwrap();
        rt.create_shader_dxbc(PS, &build_ps_passthrough_color_dxbc())
            .unwrap();
        rt.create_input_layout(IL, &build_ilay_pos3()).unwrap();

        let vertices: [VertexPos3; 3] = [
            VertexPos3 {
                pos: [-1.0, -1.0, 0.0],
            },
            VertexPos3 {
                pos: [3.0, -1.0, 0.0],
            },
            VertexPos3 {
                pos: [-1.0, 3.0, 0.0],
            },
        ];
        rt.create_buffer(
            VB,
            std::mem::size_of_val(&vertices) as u64,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(VB, 0, bytemuck::bytes_of(&vertices))
            .unwrap();

        rt.create_texture2d(
            TEX,
            2,
            2,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        );
        let tex_data: [[u8; 4]; 4] = [
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [255, 0, 0, 255],
            [0, 255, 0, 255],
        ];
        rt.write_texture_rgba8(TEX, 2, 2, 2 * 4, bytemuck::bytes_of(&tex_data))
            .unwrap();
        rt.set_vs_texture(0, Some(TEX));

        rt.create_texture2d(
            RTEX,
            1,
            1,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );

        let mut colors = [None; 8];
        colors[0] = Some(RTEX);
        rt.set_render_targets(&colors, None);

        rt.bind_shaders(Some(VS), Some(PS));
        rt.set_input_layout(Some(IL));
        rt.set_vertex_buffers(
            0,
            &[VertexBufferBinding {
                buffer: VB,
                stride: std::mem::size_of::<VertexPos3>() as u32,
                offset: 0,
            }],
        );
        rt.set_primitive_topology(PrimitiveTopology::TriangleList);
        rt.set_rasterizer_state(RasterizerState {
            cull_mode: None,
            front_face: wgpu::FrontFace::Ccw,
            scissor_enable: false,
        });

        // Default sampler fallback is clamp-to-edge; u=1.1 should clamp and hit the right column.
        rt.draw(3, 1, 0, 0).unwrap();
        rt.poll_wait();
        let pixels = rt.read_texture_rgba8(RTEX).await.unwrap();
        assert_eq!(pixels, vec![0, 255, 0, 255], "default clamp sampler");

        // Explicit repeat sampler should wrap u=1.1 -> 0.1 and hit the left column.
        rt.create_sampler(
            SAMPLER,
            &wgpu::SamplerDescriptor {
                label: Some("aerogpu_cmd_runtime vs_5_0 repeat sampler"),
                address_mode_u: wgpu::AddressMode::Repeat,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                address_mode_w: wgpu::AddressMode::ClampToEdge,
                mag_filter: wgpu::FilterMode::Nearest,
                min_filter: wgpu::FilterMode::Nearest,
                mipmap_filter: wgpu::FilterMode::Nearest,
                ..Default::default()
            },
        )
        .unwrap();
        rt.set_vs_sampler(0, Some(SAMPLER));
        rt.draw(3, 1, 0, 0).unwrap();
        rt.poll_wait();
        let pixels = rt.read_texture_rgba8(RTEX).await.unwrap();
        assert_eq!(pixels, vec![255, 0, 0, 255], "repeat sampler");

        // Unbound SRVs should fall back to a dummy (0,0,0,1) texture.
        rt.set_vs_texture(0, None);
        rt.draw(3, 1, 0, 0).unwrap();
        rt.poll_wait();
        let pixels = rt.read_texture_rgba8(RTEX).await.unwrap();
        assert_eq!(pixels, vec![0, 0, 0, 255], "unbound texture fallback");
    });
}

#[test]
fn aerogpu_cmd_runtime_signature_driven_texture_sampler_binding() {
    pollster::block_on(async {
        let mut rt = match AerogpuCmdRuntime::new_for_tests().await {
            Ok(rt) => rt,
            Err(err) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };

        const VS: u32 = 1;
        const PS: u32 = 2;
        const IL: u32 = 3;
        const VB: u32 = 4;
        const TEX: u32 = 5;
        const RTEX: u32 = 6;

        rt.create_shader_dxbc(VS, DXBC_VS_PASSTHROUGH_TEXCOORD)
            .unwrap();
        rt.create_shader_dxbc(PS, DXBC_PS_SAMPLE).unwrap();
        rt.create_input_layout(IL, ILAY_POS3_TEX2).unwrap();

        // Use out-of-range UVs so sampler address mode (clamp vs repeat) affects the sampled
        // texel.
        let vertices: [VertexPos3Tex2; 3] = [
            VertexPos3Tex2 {
                pos: [-1.0, -1.0, 0.0],
                tex: [1.1, 0.5],
            },
            VertexPos3Tex2 {
                pos: [3.0, -1.0, 0.0],
                tex: [1.1, 0.5],
            },
            VertexPos3Tex2 {
                pos: [-1.0, 3.0, 0.0],
                tex: [1.1, 0.5],
            },
        ];
        rt.create_buffer(
            VB,
            std::mem::size_of_val(&vertices) as u64,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(VB, 0, bytemuck::bytes_of(&vertices))
            .unwrap();

        rt.create_texture2d(
            TEX,
            2,
            2,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        );
        // 2x2 texture with left column red and right column green.
        //
        // Default sampler is clamp-to-edge, so sampling at u=1.1 clamps to u=1.0, which should hit
        // the right column (green). With a repeat sampler, u=1.1 wraps to u=0.1, hitting the left
        // column (red).
        let tex_data: [[u8; 4]; 4] = [
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [255, 0, 0, 255],
            [0, 255, 0, 255],
        ];
        rt.write_texture_rgba8(TEX, 2, 2, 2 * 4, bytemuck::bytes_of(&tex_data))
            .unwrap();
        rt.set_ps_texture(0, Some(TEX));

        rt.create_texture2d(
            RTEX,
            1,
            1,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );

        let mut colors = [None; 8];
        colors[0] = Some(RTEX);
        rt.set_render_targets(&colors, None);

        rt.bind_shaders(Some(VS), Some(PS));
        rt.set_input_layout(Some(IL));
        rt.set_vertex_buffers(
            0,
            &[VertexBufferBinding {
                buffer: VB,
                stride: std::mem::size_of::<VertexPos3Tex2>() as u32,
                offset: 0,
            }],
        );
        rt.set_primitive_topology(PrimitiveTopology::TriangleList);
        rt.set_rasterizer_state(RasterizerState {
            cull_mode: None,
            front_face: wgpu::FrontFace::Ccw,
            scissor_enable: false,
        });

        rt.draw(3, 1, 0, 0).unwrap();
        rt.poll_wait();

        let pixels = rt.read_texture_rgba8(RTEX).await.unwrap();
        assert_eq!(pixels, vec![0, 255, 0, 255], "default clamp sampler");

        const SAMPLER: u32 = 7;
        rt.create_sampler(
            SAMPLER,
            &wgpu::SamplerDescriptor {
                label: Some("aerogpu_cmd_runtime repeat sampler"),
                address_mode_u: wgpu::AddressMode::Repeat,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                address_mode_w: wgpu::AddressMode::ClampToEdge,
                mag_filter: wgpu::FilterMode::Nearest,
                min_filter: wgpu::FilterMode::Nearest,
                mipmap_filter: wgpu::FilterMode::Nearest,
                ..Default::default()
            },
        )
        .unwrap();
        rt.set_ps_sampler(0, Some(SAMPLER));

        rt.draw(3, 1, 0, 0).unwrap();
        rt.poll_wait();
        let pixels = rt.read_texture_rgba8(RTEX).await.unwrap();
        assert_eq!(pixels, vec![255, 0, 0, 255], "repeat sampler");

        // Unbound SRVs should behave like a dummy (0,0,0,1) texture in D3D.
        rt.set_ps_texture(0, None);
        rt.draw(3, 1, 0, 0).unwrap();
        rt.poll_wait();
        let pixels = rt.read_texture_rgba8(RTEX).await.unwrap();
        assert_eq!(pixels, vec![0, 0, 0, 255], "unbound texture fallback");
    });
}
