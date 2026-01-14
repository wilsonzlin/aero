mod common;

use aero_d3d11::input_layout::fnv1a_32;
use aero_d3d11::runtime::aerogpu_execute::AerogpuCmdRuntime;
use aero_d3d11::runtime::aerogpu_state::{PrimitiveTopology, RasterizerState, VertexBufferBinding};
use aero_dxbc::{test_utils as dxbc_test_utils, FourCC};

const DXBC_VS_MATRIX: &[u8] = include_bytes!("fixtures/vs_matrix.dxbc");
const DXBC_VS_PASSTHROUGH: &[u8] = include_bytes!("fixtures/vs_passthrough.dxbc");
const DXBC_VS_PASSTHROUGH_TEXCOORD: &[u8] = include_bytes!("fixtures/vs_passthrough_texcoord.dxbc");
const DXBC_PS_ADD: &[u8] = include_bytes!("fixtures/ps_add.dxbc");
const DXBC_PS_LD: &[u8] = include_bytes!("fixtures/ps_ld.dxbc");
const DXBC_PS_SAMPLE: &[u8] = include_bytes!("fixtures/ps_sample.dxbc");
const ILAY_POS3_COLOR: &[u8] = include_bytes!("fixtures/ilay_pos3_color.bin");
const ILAY_POS3_TEX2: &[u8] = include_bytes!("fixtures/ilay_pos3_tex2.bin");

fn build_dxbc(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
    dxbc_test_utils::build_container_owned(chunks)
}

#[derive(Clone, Copy)]
struct SigParam {
    semantic_name: &'static str,
    semantic_index: u32,
    register: u32,
    mask: u8,
}

fn build_signature_chunk(params: &[SigParam]) -> Vec<u8> {
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

fn build_signature_chunk_v1(params: &[SigParam]) -> Vec<u8> {
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
    dxbc_test_utils::build_signature_chunk_v1(&entries)
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

    build_dxbc(&[
        (FourCC(*b"ISGN"), isgn),
        (FourCC(*b"OSGN"), osgn),
        (FourCC(*b"SHDR"), shdr),
    ])
}

fn build_ps_solid_green_rgb_only_output_dxbc() -> Vec<u8> {
    // Like `build_ps_solid_red_dxbc`, but:
    // - Writes only RGB (o0.xyz) and leaves alpha unwritten.
    // - Declares the SV_Target0 signature mask as RGB-only.
    //
    // This models a pixel shader returning a float3 color. D3D fills the missing alpha component
    // with 1.0 when writing to an RGBA render target.
    //
    // Token stream (SM4 subset):
    //   mov o0.xyz, l(0, 1, 0, 0)
    //   ret
    let isgn = build_signature_chunk(&[]);
    let osgn = build_signature_chunk(&[SigParam {
        semantic_name: "SV_Target",
        semantic_index: 0,
        register: 0,
        mask: 0x07, // RGB only
    }]);

    let version_token = 0x40u32; // ps_4_0
    let mov_token = 0x01u32 | (8u32 << 11);
    let ret_token = 0x3eu32 | (1u32 << 11);

    // Destination operand with write mask XYZ.
    let dst_o0_xyz = 0x0010_7022u32;
    let imm_vec4 = 0x0000_f042u32;

    let zero = 0.0f32.to_bits();
    let one = 1.0f32.to_bits();

    let mut tokens = vec![
        version_token,
        0, // length patched below
        mov_token,
        dst_o0_xyz,
        0, // o0 index
        imm_vec4,
        zero,
        one,
        zero,
        zero,
        ret_token,
    ];
    tokens[1] = tokens.len() as u32;

    let mut shdr = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        shdr.extend_from_slice(&t.to_le_bytes());
    }

    build_dxbc(&[
        (FourCC(*b"ISGN"), isgn),
        (FourCC(*b"OSGN"), osgn),
        (FourCC(*b"SHDR"), shdr),
    ])
}

fn build_ps_solid_red_with_unused_color_input_dxbc() -> Vec<u8> {
    // Like `build_ps_solid_red_dxbc`, but declares an unused COLOR0 input at v1.
    //
    // This is a regression test helper for the VS↔PS linker: WebGPU requires that the fragment
    // stage only declares `@location`s that the vertex stage actually outputs, but D3D allows
    // unused signature parameters.
    let isgn = build_signature_chunk(&[SigParam {
        semantic_name: "COLOR",
        semantic_index: 0,
        register: 1,
        mask: 0x0f,
    }]);
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

    build_dxbc(&[
        (FourCC(*b"ISGN"), isgn),
        (FourCC(*b"OSGN"), osgn),
        (FourCC(*b"SHDR"), shdr),
    ])
}

fn build_ps_cbuffer0_dxbc() -> Vec<u8> {
    // Hand-authored minimal DXBC container: ISGN(empty) + OSGN(SV_Target0) +
    // SHDR(token stream).
    //
    // Token stream (SM4 subset):
    //   mov o0, cb0[0]
    //   ret
    //
    // The shader does not consume any inputs, but we still include an ISGN chunk so the runtime
    // selects the signature-driven translator instead of the bootstrap fallback (which does not
    // support constant buffers).
    let isgn = build_signature_chunk(&[]);
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

    build_dxbc(&[
        (FourCC(*b"ISGN"), isgn),
        (FourCC(*b"OSGN"), osgn),
        (FourCC(*b"SHDR"), shdr),
    ])
}

fn build_ps_cbuffer0_sm5_dxbc() -> Vec<u8> {
    // Same as `build_ps_cbuffer0_dxbc`, but encoded as SM5 (`SHEX`, ps_5_0).
    let isgn = build_signature_chunk(&[]);
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

    build_dxbc(&[
        (FourCC(*b"ISGN"), isgn),
        (FourCC(*b"OSGN"), osgn),
        (FourCC(*b"SHEX"), shex),
    ])
}

fn build_ps_cbuffer_sm5_dxbc(slot: u32, reg: u32) -> Vec<u8> {
    // Generalized variant of `build_ps_cbuffer0_sm5_dxbc` that reads from `cb{slot}[{reg}]`.
    let isgn = build_signature_chunk(&[]);
    let osgn = build_signature_chunk(&[SigParam {
        semantic_name: "SV_Target",
        semantic_index: 0,
        register: 0,
        mask: 0x0f,
    }]);

    // ps_5_0
    let version_token = 0x50u32;

    // mov o0, cb#[reg]
    let mov_token = 0x01u32 | (6u32 << 11);
    let dst_o0 = 0x0010_f022u32;
    let cb = 0x002e_4086u32;
    let ret_token = 0x3eu32 | (1u32 << 11);

    let mut tokens = vec![
        version_token,
        0, // length patched below
        mov_token,
        dst_o0,
        0, // o0 index
        cb,
        slot,
        reg,
        ret_token,
    ];
    tokens[1] = tokens.len() as u32;

    let mut shex = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        shex.extend_from_slice(&t.to_le_bytes());
    }

    build_dxbc(&[
        (FourCC(*b"ISGN"), isgn),
        (FourCC(*b"OSGN"), osgn),
        (FourCC(*b"SHEX"), shex),
    ])
}

fn build_ps_two_constant_buffers_sm5_dxbc() -> Vec<u8> {
    // Minimal PS (ps_5_0) that references two constant buffers and returns the second:
    //
    // Token stream:
    //   mov o0, cb0[0]
    //   mov o0, cb1[0]
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

    let mov_token = 0x01u32 | (6u32 << 11);
    let dst_o0 = 0x0010_f022u32;
    let cb = 0x002e_4086u32;
    let ret_token = 0x3eu32 | (1u32 << 11);

    let mut tokens = vec![
        version_token,
        0, // length patched below
        // mov o0, cb0[0]
        mov_token,
        dst_o0,
        0,
        cb,
        0,
        0,
        // mov o0, cb1[0]
        mov_token,
        dst_o0,
        0,
        cb,
        1,
        0,
        ret_token,
    ];
    tokens[1] = tokens.len() as u32;

    let mut shex = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        shex.extend_from_slice(&t.to_le_bytes());
    }

    build_dxbc(&[
        (FourCC(*b"ISGN"), isgn),
        (FourCC(*b"OSGN"), osgn),
        (FourCC(*b"SHEX"), shex),
    ])
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

    build_dxbc(&[
        (FourCC(*b"ISGN"), isgn),
        (FourCC(*b"OSGN"), osgn),
        (FourCC(*b"SHEX"), shex),
    ])
}

fn build_vs_passthrough_pos_sm5_sig_v1_dxbc() -> Vec<u8> {
    // Same as `build_vs_passthrough_pos_sm5_dxbc`, but uses `ISG1`/`OSG1` signature chunks with
    // the 32-byte v1 entry layout.
    let isgn = build_signature_chunk_v1(&[SigParam {
        semantic_name: "POSITION",
        semantic_index: 0,
        register: 0,
        mask: 0x07,
    }]);
    let osgn = build_signature_chunk_v1(&[SigParam {
        semantic_name: "SV_Position",
        semantic_index: 0,
        register: 0,
        mask: 0x0f,
    }]);

    let version_token = 0x0001_0050u32; // vs_5_0
    let mov_token = 0x01u32 | (5u32 << 11);
    let dst_o0 = 0x0010_f022u32;
    let src_v0 = 0x001e_4016u32;
    let ret_token = 0x3eu32 | (1u32 << 11);

    let mut tokens = vec![
        version_token,
        0, // length patched below
        mov_token,
        dst_o0,
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

    build_dxbc(&[
        (FourCC(*b"ISG1"), isgn),
        (FourCC(*b"OSG1"), osgn),
        (FourCC(*b"SHEX"), shex),
    ])
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

    build_dxbc(&[
        (FourCC(*b"ISGN"), isgn),
        (FourCC(*b"OSGN"), osgn),
        (FourCC(*b"SHEX"), shex),
    ])
}

fn build_ps_sample_l_sm5_dxbc(u: f32, v: f32, tex_slot: u32, sampler_slot: u32) -> Vec<u8> {
    // Variant of `build_ps_sample_l_t0_s0_sm5_dxbc` that samples from a specific texture/sampler
    // slot. This is useful for exercising the binding model at non-zero / max slot indices.
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
    let t = 0x0010_0072u32;
    let s = 0x0010_0062u32;

    let u = u.to_bits();
    let v = v.to_bits();

    let mut tokens = vec![
        version_token,
        0, // length patched below
        // sample_l o0, l(u,v,0,0), t#, s#, l(0)
        sample_l_opcode_token,
        dst_o0,
        0, // o0 index
        imm_vec4,
        u,
        v,
        0,
        0,
        t,
        tex_slot,
        s,
        sampler_slot,
        imm_scalar,
        0, // lod=0
        ret_token,
    ];
    tokens[1] = tokens.len() as u32;

    let mut shex = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        shex.extend_from_slice(&t.to_le_bytes());
    }

    build_dxbc(&[
        (FourCC(*b"ISGN"), isgn),
        (FourCC(*b"OSGN"), osgn),
        (FourCC(*b"SHEX"), shex),
    ])
}

fn build_ps_cbuffer0_and_sample_l_t0_s0_sm5_dxbc(u: f32, v: f32) -> Vec<u8> {
    // Minimal PS (ps_5_0) that references both cb0 and texture sampling (t0/s0). The constant
    // buffer read is intentionally overwritten so the final color comes from the texture sample;
    // this drives combined resource bindings in the PS bind group without needing additional ALU
    // instructions.
    //
    // Token stream:
    //   mov o0, cb0[0]
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

    let mov_token = 0x01u32 | (6u32 << 11);
    let sample_l_opcode_token = 0x46u32 | (14u32 << 11);
    let ret_token = 0x3eu32 | (1u32 << 11);

    let dst_o0 = 0x0010_f022u32;
    let cb0_reg0 = 0x002e_4086u32;
    let imm_vec4 = 0x0000_f042u32;
    let imm_scalar = 0x0000_0049u32;
    let t0 = 0x0010_0072u32;
    let s0 = 0x0010_0062u32;

    let u = u.to_bits();
    let v = v.to_bits();

    let mut tokens = vec![
        version_token,
        0, // length patched below
        // mov o0, cb0[0]
        mov_token,
        dst_o0,
        0,
        cb0_reg0,
        0,
        0,
        // sample_l o0, l(u,v,0,0), t0, s0, l(0)
        sample_l_opcode_token,
        dst_o0,
        0,
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
        0,
        ret_token,
    ];
    tokens[1] = tokens.len() as u32;

    let mut shex = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        shex.extend_from_slice(&t.to_le_bytes());
    }

    build_dxbc(&[
        (FourCC(*b"ISGN"), isgn),
        (FourCC(*b"OSGN"), osgn),
        (FourCC(*b"SHEX"), shex),
    ])
}

fn build_ps_two_sample_l_sm5_dxbc(u: f32, v: f32) -> Vec<u8> {
    // PS that performs two independent samples from (t0,s0) and (t1,s1) and returns the second.
    // This exercises multiple texture/sampler bindings in the same stage.
    //
    // Token stream:
    //   sample_l o0, l(u, v, 0, 0), t0, s0, l(0)
    //   sample_l o0, l(u, v, 0, 0), t1, s1, l(0)
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
    let t = 0x0010_0072u32;
    let s = 0x0010_0062u32;

    let u = u.to_bits();
    let v = v.to_bits();

    let mut tokens = vec![
        version_token,
        0, // length patched below
        // sample_l o0, l(u,v,0,0), t0, s0, l(0)
        sample_l_opcode_token,
        dst_o0,
        0,
        imm_vec4,
        u,
        v,
        0,
        0,
        t,
        0,
        s,
        0,
        imm_scalar,
        0,
        // sample_l o0, l(u,v,0,0), t1, s1, l(0)
        sample_l_opcode_token,
        dst_o0,
        0,
        imm_vec4,
        u,
        v,
        0,
        0,
        t,
        1,
        s,
        1,
        imm_scalar,
        0,
        ret_token,
    ];
    tokens[1] = tokens.len() as u32;

    let mut shex = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        shex.extend_from_slice(&t.to_le_bytes());
    }

    build_dxbc(&[
        (FourCC(*b"ISGN"), isgn),
        (FourCC(*b"OSGN"), osgn),
        (FourCC(*b"SHEX"), shex),
    ])
}

fn build_ps_ld_t0_sm5_dxbc(x: i32, y: i32, mip: i32) -> Vec<u8> {
    // Minimal PS (ps_5_0) that performs a `Texture2D.Load` style `ld` from t0 and returns it as
    // SV_Target0.
    //
    // Token stream:
    //   ld o0, l(x, y, mip, 0), t0
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

    // ld opcode (0x4c). Instruction length = 10 dwords:
    //   opcode + dst(2) + coord(1+4) + resource(2)
    let ld_token = 0x4cu32 | (10u32 << 11);
    let ret_token = 0x3eu32 | (1u32 << 11);

    let dst_o0 = 0x0010_f022u32;
    let imm_vec4 = 0x0000_f042u32;
    let t0 = 0x0010_0072u32;

    let mut tokens = vec![
        version_token,
        0, // length patched below
        // ld o0, l(x, y, mip, 0), t0
        ld_token,
        dst_o0,
        0, // o0 index
        imm_vec4,
        x as u32,
        y as u32,
        mip as u32,
        0,
        t0,
        0, // t0 slot
        ret_token,
    ];
    tokens[1] = tokens.len() as u32;

    let mut shex = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        shex.extend_from_slice(&t.to_le_bytes());
    }

    build_dxbc(&[
        (FourCC(*b"ISGN"), isgn),
        (FourCC(*b"OSGN"), osgn),
        (FourCC(*b"SHEX"), shex),
    ])
}

fn build_ps_cbuffer0_sm5_sig_v1_dxbc() -> Vec<u8> {
    // Equivalent to `build_ps_cbuffer0_sm5_dxbc`, but uses `ISG1`/`OSG1` signature chunks with the
    // 32-byte v1 entry layout.
    // Keep a non-empty ISG1 chunk so the v1 parsing path is exercised, but do not require any
    // interpolant locations.
    let isgn = build_signature_chunk_v1(&[SigParam {
        semantic_name: "SV_Position",
        semantic_index: 0,
        register: 0,
        mask: 0x0f,
    }]);
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

    build_dxbc(&[
        (FourCC(*b"ISG1"), isgn),
        (FourCC(*b"OSG1"), osgn),
        (FourCC(*b"SHEX"), shex),
    ])
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

    build_dxbc(&[
        (FourCC(*b"ISGN"), isgn),
        (FourCC(*b"OSGN"), osgn),
        (FourCC(*b"SHDR"), shdr),
    ])
}

fn build_vs_passthrough_pos_and_color_with_rgb_mask_dxbc() -> Vec<u8> {
    // Minimal VS that passes POSITION0 (v0.xyz) to SV_Position (o0) and COLOR0 (v1.xyzw) to COLOR0
    // (o1), but declares the output COLOR0 mask as RGB-only (0x07).
    //
    // This models a common D3D pattern where one stage treats a varying as float3 while the other
    // stage treats it as float4. D3D allows this, but WebGPU requires exact type matching at
    // `@location(1)`.
    //
    // Token stream (SM4 subset):
    //   mov o0, v0
    //   mov o1, v1
    //   ret
    let isgn = build_signature_chunk(&[
        SigParam {
            semantic_name: "POSITION",
            semantic_index: 0,
            register: 0,
            mask: 0x07,
        },
        SigParam {
            semantic_name: "COLOR",
            semantic_index: 0,
            register: 1,
            mask: 0x0f,
        },
    ]);
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
            // RGB only.
            mask: 0x07,
        },
    ]);

    // vs_4_0
    let version_token = 0x0001_0040u32;
    // mov o#, v#
    let mov_token = 0x01u32 | (5u32 << 11);
    let dst_o = 0x0010_f022u32;
    let src_v = 0x001e_4016u32;
    let ret_token = 0x3eu32 | (1u32 << 11);

    let mut tokens = vec![
        version_token,
        0, // length patched below
        // mov o0, v0
        mov_token,
        dst_o,
        0, // o0 index
        src_v,
        0, // v0 index
        // mov o1, v1
        mov_token,
        dst_o,
        1, // o1 index
        src_v,
        1, // v1 index
        ret_token,
    ];
    tokens[1] = tokens.len() as u32;

    let mut shdr = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        shdr.extend_from_slice(&t.to_le_bytes());
    }

    build_dxbc(&[
        (FourCC(*b"ISGN"), isgn),
        (FourCC(*b"OSGN"), osgn),
        (FourCC(*b"SHDR"), shdr),
    ])
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

    build_dxbc(&[
        (FourCC(*b"ISGN"), isgn),
        (FourCC(*b"OSGN"), osgn),
        (FourCC(*b"SHDR"), shdr),
    ])
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

    build_dxbc(&[
        (FourCC(*b"ISGN"), isgn),
        (FourCC(*b"OSGN"), osgn),
        (FourCC(*b"SHEX"), shex),
    ])
}

fn build_vs_two_sample_l_sm5_dxbc(u: f32, v: f32) -> Vec<u8> {
    // SM5 VS that samples twice:
    // - first from t0/s0
    // - then from t1/s1 (overwriting output)
    //
    // This exercises multiple texture/sampler bindings in the VS bind group.
    //
    // Token stream:
    //   sample_l o1, l(u, v, 0, 0), t0, s0, l(0)
    //   sample_l o1, l(u, v, 0, 0), t1, s1, l(0)
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

    // vs_5_0
    let version_token = 0x0001_0050u32;
    let sample_l_opcode_token = 0x46u32 | (14u32 << 11);
    let mov_token = 0x01u32 | (5u32 << 11);
    let ret_token = 0x3eu32 | (1u32 << 11);

    let dst_o = 0x0010_f022u32;
    let imm_vec4 = 0x0000_f042u32;
    let imm_scalar = 0x0000_0049u32;
    let t = 0x0010_0072u32;
    let s = 0x0010_0062u32;
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
        t,
        0,
        s,
        0,
        imm_scalar,
        0,
        // sample_l o1, l(u,v,0,0), t1, s1, l(0)
        sample_l_opcode_token,
        dst_o,
        1, // o1 index
        imm_vec4,
        u,
        v,
        0,
        0,
        t,
        1,
        s,
        1,
        imm_scalar,
        0,
        // mov o0, v0
        mov_token,
        dst_o,
        0,
        src_v0,
        0,
        ret_token,
    ];
    tokens[1] = tokens.len() as u32;

    let mut shex = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        shex.extend_from_slice(&t.to_le_bytes());
    }

    build_dxbc(&[
        (FourCC(*b"ISGN"), isgn),
        (FourCC(*b"OSGN"), osgn),
        (FourCC(*b"SHEX"), shex),
    ])
}

fn build_vs_ld_t0_to_color1_sm5_dxbc(x: i32, y: i32, mip: i32) -> Vec<u8> {
    // SM5 VS that performs a texture load (`ld` / `textureLoad`) from t0 and outputs it as COLOR0
    // (o1). Position is passed through from input POSITION0 (v0) to SV_Position (o0).
    //
    // Token stream:
    //   ld o1, l(x, y, mip, 0), t0
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

    // vs_5_0
    let version_token = 0x0001_0050u32;

    let ld_token = 0x4cu32 | (10u32 << 11);
    let mov_token = 0x01u32 | (5u32 << 11);
    let ret_token = 0x3eu32 | (1u32 << 11);

    let dst_o = 0x0010_f022u32;
    let imm_vec4 = 0x0000_f042u32;
    let t0 = 0x0010_0072u32;
    let src_v0 = 0x001e_4016u32;

    let mut tokens = vec![
        version_token,
        0, // length patched below
        // ld o1, l(x,y,mip,0), t0
        ld_token,
        dst_o,
        1, // o1
        imm_vec4,
        x as u32,
        y as u32,
        mip as u32,
        0,
        t0,
        0, // t0 slot
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

    build_dxbc(&[
        (FourCC(*b"ISGN"), isgn),
        (FourCC(*b"OSGN"), osgn),
        (FourCC(*b"SHEX"), shex),
    ])
}

fn build_vs_matrix_sample_t0_s0_sm5_dxbc(u: f32, v: f32) -> Vec<u8> {
    // SM5 VS that uses *both* a constant buffer and a texture+sampler:
    // - SV_Position (o0) = POSITION0 (v0) transformed by cb0[0..3] (4x4 matrix)
    // - COLOR0 (o1) = sample_l(t0, s0) at a constant UV
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
    let dp4_token = 0x09u32 | (8u32 << 11);
    let sample_l_opcode_token = 0x46u32 | (14u32 << 11);
    let ret_token = 0x3eu32 | (1u32 << 11);

    let dst_o0_x = 0x0010_1022u32;
    let dst_o0_y = 0x0010_2022u32;
    let dst_o0_z = 0x0010_4022u32;
    let dst_o0_w = 0x0010_8022u32;
    let dst_o = 0x0010_f022u32;

    let src_v0 = 0x001e_4016u32;
    let cb = 0x002e_4086u32;
    let imm_vec4 = 0x0000_f042u32;
    let imm_scalar = 0x0000_0049u32;
    let t0 = 0x0010_0072u32;
    let s0 = 0x0010_0062u32;

    let u = u.to_bits();
    let v = v.to_bits();

    let mut tokens = vec![
        version_token,
        0, // length patched below
        // dp4 o0.x, v0, cb0[0]
        dp4_token,
        dst_o0_x,
        0,
        src_v0,
        0,
        cb,
        0,
        0,
        // dp4 o0.y, v0, cb0[1]
        dp4_token,
        dst_o0_y,
        0,
        src_v0,
        0,
        cb,
        0,
        1,
        // dp4 o0.z, v0, cb0[2]
        dp4_token,
        dst_o0_z,
        0,
        src_v0,
        0,
        cb,
        0,
        2,
        // dp4 o0.w, v0, cb0[3]
        dp4_token,
        dst_o0_w,
        0,
        src_v0,
        0,
        cb,
        0,
        3,
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
        0,
        ret_token,
    ];
    tokens[1] = tokens.len() as u32;

    let mut shex = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        shex.extend_from_slice(&t.to_le_bytes());
    }

    build_dxbc(&[
        (FourCC(*b"ISGN"), isgn),
        (FourCC(*b"OSGN"), osgn),
        (FourCC(*b"SHEX"), shex),
    ])
}

fn build_vs_matrix_texcoord_dxbc() -> Vec<u8> {
    // Minimal VS that:
    // - Multiplies POSITION0 (v0.xyz, with implicit w=1) by cb0[0..3] into SV_Position (o1).
    // - Passes TEXCOORD0 (v1.xy) through to TEXCOORD0 (o0).
    //
    // Token stream (SM4 subset):
    //   dp4 o1.x, v0, cb0[0]
    //   dp4 o1.y, v0, cb0[1]
    //   dp4 o1.z, v0, cb0[2]
    //   dp4 o1.w, v0, cb0[3]
    //   mov o0, v1
    //   ret
    //
    // This ensures the runtime must bind resources for *both* stage-scoped groups:
    // - group 0 (VS) cb0
    // - group 1 (PS) t0+s0 (when paired with `ps_sample.dxbc`)
    let isgn = build_signature_chunk(&[
        SigParam {
            semantic_name: "POSITION",
            semantic_index: 0,
            register: 0,
            mask: 0x07,
        },
        SigParam {
            semantic_name: "TEXCOORD",
            semantic_index: 0,
            register: 1,
            mask: 0x03,
        },
    ]);
    let osgn = build_signature_chunk(&[
        SigParam {
            semantic_name: "TEXCOORD",
            semantic_index: 0,
            register: 0,
            mask: 0x03,
        },
        SigParam {
            semantic_name: "SV_Position",
            semantic_index: 0,
            register: 1,
            mask: 0x0f,
        },
    ]);

    // vs_4_0
    let version_token = 0x0001_0040u32;

    let dp4_token = 0x09u32 | (8u32 << 11);
    let mov_token = 0x01u32 | (5u32 << 11);
    let ret_token = 0x3eu32 | (1u32 << 11);

    let dst_o1_x = 0x0010_1022u32;
    let dst_o1_y = 0x0010_2022u32;
    let dst_o1_z = 0x0010_4022u32;
    let dst_o1_w = 0x0010_8022u32;
    let dst_o0 = 0x0010_f022u32;

    let src_v0 = 0x001e_4016u32;
    let src_v1 = 0x001e_4016u32;
    let cb = 0x002e_4086u32;

    let mut tokens = vec![
        version_token,
        0, // length patched below
        // dp4 o1.x, v0, cb0[0]
        dp4_token,
        dst_o1_x,
        1, // o1
        src_v0,
        0, // v0
        cb,
        0, // cb slot
        0, // cb reg
        // dp4 o1.y, v0, cb0[1]
        dp4_token,
        dst_o1_y,
        1,
        src_v0,
        0,
        cb,
        0,
        1,
        // dp4 o1.z, v0, cb0[2]
        dp4_token,
        dst_o1_z,
        1,
        src_v0,
        0,
        cb,
        0,
        2,
        // dp4 o1.w, v0, cb0[3]
        dp4_token,
        dst_o1_w,
        1,
        src_v0,
        0,
        cb,
        0,
        3,
        // mov o0, v1
        mov_token,
        dst_o0,
        0, // o0
        src_v1,
        1, // v1
        ret_token,
    ];
    tokens[1] = tokens.len() as u32;

    let mut shdr = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        shdr.extend_from_slice(&t.to_le_bytes());
    }

    build_dxbc(&[
        (FourCC(*b"ISGN"), isgn),
        (FourCC(*b"OSGN"), osgn),
        (FourCC(*b"SHDR"), shdr),
    ])
}

fn build_vs_matrix_pos_cb_slot_sm5_dxbc(cb_slot: u32) -> Vec<u8> {
    // Minimal VS (vs_5_0) that multiplies POSITION0 by `cb{cb_slot}[0..3]` into SV_Position (o0).
    //
    // Token stream:
    //   dp4 o0.x, v0, cb#[0]
    //   dp4 o0.y, v0, cb#[1]
    //   dp4 o0.z, v0, cb#[2]
    //   dp4 o0.w, v0, cb#[3]
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

    // vs_5_0
    let version_token = 0x0001_0050u32;
    let dp4_token = 0x09u32 | (8u32 << 11);
    let ret_token = 0x3eu32 | (1u32 << 11);

    let dst_o_x = 0x0010_1022u32;
    let dst_o_y = 0x0010_2022u32;
    let dst_o_z = 0x0010_4022u32;
    let dst_o_w = 0x0010_8022u32;

    let src_v0 = 0x001e_4016u32;
    let cb = 0x002e_4086u32;

    let mut tokens = vec![
        version_token,
        0, // length patched below
        // dp4 o0.x, v0, cb#[0]
        dp4_token,
        dst_o_x,
        0, // o0
        src_v0,
        0, // v0
        cb,
        cb_slot,
        0,
        // dp4 o0.y, v0, cb#[1]
        dp4_token,
        dst_o_y,
        0,
        src_v0,
        0,
        cb,
        cb_slot,
        1,
        // dp4 o0.z, v0, cb#[2]
        dp4_token,
        dst_o_z,
        0,
        src_v0,
        0,
        cb,
        cb_slot,
        2,
        // dp4 o0.w, v0, cb#[3]
        dp4_token,
        dst_o_w,
        0,
        src_v0,
        0,
        cb,
        cb_slot,
        3,
        ret_token,
    ];
    tokens[1] = tokens.len() as u32;

    let mut shex = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        shex.extend_from_slice(&t.to_le_bytes());
    }

    build_dxbc(&[
        (FourCC(*b"ISGN"), isgn),
        (FourCC(*b"OSGN"), osgn),
        (FourCC(*b"SHEX"), shex),
    ])
}

fn build_vs_matrix_pos_cb0_and_color_cb1_sm5_dxbc() -> Vec<u8> {
    // Vertex shader that references two constant buffers:
    // - cb0 provides a 4x4 transform matrix (regs 0..3) applied to POSITION0 for SV_Position (o0)
    // - cb1[0] provides a constant color written to COLOR0 (o1)
    //
    // Token stream:
    //   dp4 o0.x, v0, cb0[0]
    //   dp4 o0.y, v0, cb0[1]
    //   dp4 o0.z, v0, cb0[2]
    //   dp4 o0.w, v0, cb0[3]
    //   mov o1, cb1[0]
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

    // vs_5_0
    let version_token = 0x0001_0050u32;
    let dp4_token = 0x09u32 | (8u32 << 11);
    let mov_token = 0x01u32 | (6u32 << 11);
    let ret_token = 0x3eu32 | (1u32 << 11);

    let dst_o_x = 0x0010_1022u32;
    let dst_o_y = 0x0010_2022u32;
    let dst_o_z = 0x0010_4022u32;
    let dst_o_w = 0x0010_8022u32;
    let dst_o = 0x0010_f022u32;

    let src_v0 = 0x001e_4016u32;
    let cb = 0x002e_4086u32;

    let mut tokens = vec![
        version_token,
        0, // length patched below
        // dp4 o0.x, v0, cb0[0]
        dp4_token,
        dst_o_x,
        0,
        src_v0,
        0,
        cb,
        0,
        0,
        // dp4 o0.y, v0, cb0[1]
        dp4_token,
        dst_o_y,
        0,
        src_v0,
        0,
        cb,
        0,
        1,
        // dp4 o0.z, v0, cb0[2]
        dp4_token,
        dst_o_z,
        0,
        src_v0,
        0,
        cb,
        0,
        2,
        // dp4 o0.w, v0, cb0[3]
        dp4_token,
        dst_o_w,
        0,
        src_v0,
        0,
        cb,
        0,
        3,
        // mov o1, cb1[0]
        mov_token,
        dst_o,
        1, // o1
        cb,
        1, // cb1 slot
        0, // reg0
        ret_token,
    ];
    tokens[1] = tokens.len() as u32;

    let mut shex = Vec::with_capacity(tokens.len() * 4);
    for t in tokens {
        shex.extend_from_slice(&t.to_le_bytes());
    }

    build_dxbc(&[
        (FourCC(*b"ISGN"), isgn),
        (FourCC(*b"OSGN"), osgn),
        (FourCC(*b"SHEX"), shex),
    ])
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
        rt.create_shader_dxbc(PS, &build_ps_solid_red_dxbc())
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

        rt.bind_shaders(Some(VS), None, Some(PS));
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
fn aerogpu_cmd_runtime_signature_driven_vs_two_constant_buffers_sm5() {
    // Ensure signature-driven runtime bindings support multiple constant buffers within the same
    // stage-scoped bind group (VS cb0 + cb1).
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
        const CB1: u32 = 6;
        const RTEX: u32 = 7;

        rt.create_shader_dxbc(VS, &build_vs_matrix_pos_cb0_and_color_cb1_sm5_dxbc())
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

        // cb1[0] = solid blue
        let cb1_color: [f32; 4] = [0.0, 0.0, 1.0, 1.0];
        rt.create_buffer(
            CB1,
            std::mem::size_of_val(&cb1_color) as u64,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(CB1, 0, bytemuck::bytes_of(&cb1_color))
            .unwrap();
        rt.set_vs_constant_buffer(1, Some(CB1));

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

        rt.bind_shaders(Some(VS), None, Some(PS));
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
        assert_eq!(pixels, vec![0, 0, 255, 255]);
    });
}

#[test]
fn aerogpu_cmd_runtime_signature_driven_ps_add_sat_fixture() {
    // End-to-end test for the checked-in SM4 `ps_add.dxbc` fixture (`add_sat o0, v1, v1`).
    //
    // This exercises ALU translation + saturate handling (clamp to [0,1]) in the signature-driven
    // SM4→WGSL path.
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
        const RTEX: u32 = 5;

        rt.create_shader_dxbc(VS, DXBC_VS_PASSTHROUGH).unwrap();
        rt.create_shader_dxbc(PS, DXBC_PS_ADD).unwrap();
        rt.create_input_layout(IL, ILAY_POS3_COLOR).unwrap();

        // Input color is 0.75; add_sat doubles it to 1.5 and clamps to 1.0 (full red).
        let vertices: [VertexPos3Color4; 3] = [
            VertexPos3Color4 {
                pos: [-1.0, -1.0, 0.0],
                color: [0.75, 0.0, 0.0, 1.0],
            },
            VertexPos3Color4 {
                pos: [3.0, -1.0, 0.0],
                color: [0.75, 0.0, 0.0, 1.0],
            },
            VertexPos3Color4 {
                pos: [-1.0, 3.0, 0.0],
                color: [0.75, 0.0, 0.0, 1.0],
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
            RTEX,
            4,
            4,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let mut colors = [None; 8];
        colors[0] = Some(RTEX);
        rt.set_render_targets(&colors, None);

        rt.bind_shaders(Some(VS), None, Some(PS));
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
            assert_eq!(px, &[255, 0, 0, 255], "pixel index {i}");
        }
    });
}

#[test]
fn aerogpu_cmd_runtime_signature_driven_vs_sig_v1_position_binding() {
    // End-to-end regression test for parsing signature-v1 (`ISG1`/`OSG1`) chunks in a vertex shader
    // and using the resulting signature for input-layout mapping.
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
        const RTEX: u32 = 5;

        rt.create_shader_dxbc(VS, &build_vs_passthrough_pos_sm5_sig_v1_dxbc())
            .unwrap();
        rt.create_shader_dxbc(PS, &build_ps_solid_red_dxbc())
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
            RTEX,
            4,
            4,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let mut colors = [None; 8];
        colors[0] = Some(RTEX);
        rt.set_render_targets(&colors, None);

        rt.bind_shaders(Some(VS), None, Some(PS));
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
fn aerogpu_cmd_runtime_signature_driven_vs_cb0_and_ps_texture_binding() {
    // Ensure signature-driven runtime bindings work when *both* stages declare resources:
    // - VS: constant buffer cb0
    // - PS: texture t0 + sampler s0
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
        const TEX: u32 = 6;
        const RTEX: u32 = 7;

        rt.create_shader_dxbc(VS, &build_vs_matrix_texcoord_dxbc())
            .unwrap();
        rt.create_shader_dxbc(PS, DXBC_PS_SAMPLE).unwrap();
        rt.create_input_layout(IL, ILAY_POS3_TEX2).unwrap();

        // Fullscreen triangle with UVs.
        let vertices: [VertexPos3Tex2; 3] = [
            VertexPos3Tex2 {
                pos: [-1.0, -1.0, 0.0],
                tex: [0.0, 0.0],
            },
            VertexPos3Tex2 {
                pos: [3.0, -1.0, 0.0],
                tex: [2.0, 0.0],
            },
            VertexPos3Tex2 {
                pos: [-1.0, 3.0, 0.0],
                tex: [0.0, 2.0],
            },
        ];
        rt.create_buffer(
            VB,
            std::mem::size_of_val(&vertices) as u64,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(VB, 0, bytemuck::bytes_of(&vertices))
            .unwrap();

        // cb0 = identity matrix.
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
            4,
            4,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let mut colors = [None; 8];
        colors[0] = Some(RTEX);
        rt.set_render_targets(&colors, None);

        rt.bind_shaders(Some(VS), None, Some(PS));
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
        assert_eq!(pixels.len(), 4 * 4 * 4);
        for (i, px) in pixels.chunks_exact(4).enumerate() {
            assert_eq!(px, &[0, 255, 0, 255], "pixel {i}");
        }
    });
}

#[test]
fn aerogpu_cmd_runtime_signature_driven_vs_and_ps_texture_sampler_binding_sm5() {
    // Ensure signature-driven runtime bindings work when *both* stages declare texture sampling
    // resources (t0+s0 in group0 and group1 simultaneously).
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
        const SAMPLER: u32 = 6;
        const RTEX: u32 = 7;

        rt.create_shader_dxbc(VS, &build_vs_sample_t0_s0_to_color1_sm5_dxbc(0.0, 0.0))
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
        rt.set_vs_texture(0, Some(TEX));
        rt.set_ps_texture(0, Some(TEX));

        rt.create_sampler(
            SAMPLER,
            &wgpu::SamplerDescriptor {
                label: Some("aerogpu_cmd_runtime shared sampler"),
                address_mode_u: wgpu::AddressMode::ClampToEdge,
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
        rt.set_ps_sampler(0, Some(SAMPLER));

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

        rt.bind_shaders(Some(VS), None, Some(PS));
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
fn aerogpu_cmd_runtime_signature_driven_trims_unused_ps_inputs_for_linking() {
    // Regression test: pixel shader declares an unused varying input (v1) that the bound VS does
    // not output. WebGPU rejects this unless the pipeline linker trims the unused PS input.
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
        const RTEX: u32 = 5;

        rt.create_shader_dxbc(VS, &build_vs_passthrough_pos_sm5_dxbc())
            .unwrap();
        rt.create_shader_dxbc(PS, &build_ps_solid_red_with_unused_color_input_dxbc())
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
            RTEX,
            1,
            1,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let mut colors = [None; 8];
        colors[0] = Some(RTEX);
        rt.set_render_targets(&colors, None);

        rt.bind_shaders(Some(VS), None, Some(PS));
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
        assert_eq!(pixels, vec![255, 0, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_runtime_signature_driven_links_mismatched_varying_masks() {
    // Regression test: D3D stage interfaces can legally disagree on the component mask for a given
    // varying register (e.g. VS exports float3 but PS declares float4). WebGPU requires the WGSL
    // types to match exactly at a `@location`, so we normalize varyings to `vec4<f32>`.
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
        const RTEX: u32 = 5;

        rt.create_shader_dxbc(VS, &build_vs_passthrough_pos_and_color_with_rgb_mask_dxbc())
            .unwrap();
        rt.create_shader_dxbc(PS, &build_ps_passthrough_color_dxbc())
            .unwrap();
        rt.create_input_layout(IL, ILAY_POS3_COLOR).unwrap();

        // Fullscreen triangle in clip space. The vertex color is green with alpha 0.0; the VS
        // output signature masks away alpha (RGB-only), so the PS should observe alpha=1.0.
        let vertices: [VertexPos3Color4; 3] = [
            VertexPos3Color4 {
                pos: [-1.0, -1.0, 0.0],
                color: [0.0, 1.0, 0.0, 0.0],
            },
            VertexPos3Color4 {
                pos: [3.0, -1.0, 0.0],
                color: [0.0, 1.0, 0.0, 0.0],
            },
            VertexPos3Color4 {
                pos: [-1.0, 3.0, 0.0],
                color: [0.0, 1.0, 0.0, 0.0],
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
            RTEX,
            1,
            1,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let mut colors = [None; 8];
        colors[0] = Some(RTEX);
        rt.set_render_targets(&colors, None);

        rt.bind_shaders(Some(VS), None, Some(PS));
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
        assert_eq!(pixels, vec![0, 255, 0, 255]);
    });
}

#[test]
fn aerogpu_cmd_runtime_signature_driven_fills_missing_ps_output_alpha() {
    // Regression test: if a PS output signature only covers RGB (float3), D3D fills the missing
    // alpha component with 1.0. Since our internal register file is vec4 and unwritten lanes
    // default to 0, we must apply signature-based default fill when returning SV_Target0.
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
        const RTEX: u32 = 5;

        rt.create_shader_dxbc(VS, &build_vs_passthrough_pos_sm5_dxbc())
            .unwrap();
        rt.create_shader_dxbc(PS, &build_ps_solid_green_rgb_only_output_dxbc())
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
            RTEX,
            1,
            1,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let mut colors = [None; 8];
        colors[0] = Some(RTEX);
        rt.set_render_targets(&colors, None);

        rt.bind_shaders(Some(VS), None, Some(PS));
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
fn aerogpu_cmd_runtime_signature_driven_max_slot_resource_bindings() {
    // Regression test for the binding model at the maximum supported D3D slots:
    // - VS: cb13 (binds at @binding(13) within @group(0))
    // - PS: t127 + s15 (bind at @binding(159) + @binding(175) within @group(1))
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
        const CB: u32 = 5;
        const TEX: u32 = 6;
        const SAMPLER: u32 = 7;
        const RTEX: u32 = 8;

        const VS_CB_SLOT: u32 = 13;
        const PS_TEX_SLOT: u32 = 127;
        const PS_SAMPLER_SLOT: u32 = 15;

        rt.create_shader_dxbc(VS, &build_vs_matrix_pos_cb_slot_sm5_dxbc(VS_CB_SLOT))
            .unwrap();
        rt.create_shader_dxbc(
            PS,
            &build_ps_sample_l_sm5_dxbc(0.0, 0.0, PS_TEX_SLOT, PS_SAMPLER_SLOT),
        )
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

        let identity: [[f32; 4]; 4] = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        rt.create_buffer(
            CB,
            std::mem::size_of_val(&identity) as u64,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(CB, 0, bytemuck::bytes_of(&identity))
            .unwrap();
        rt.set_vs_constant_buffer(VS_CB_SLOT, Some(CB));

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
        rt.set_ps_texture(PS_TEX_SLOT, Some(TEX));

        rt.create_sampler(
            SAMPLER,
            &wgpu::SamplerDescriptor {
                label: Some("aerogpu_cmd_runtime max-slot sampler"),
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                address_mode_w: wgpu::AddressMode::ClampToEdge,
                mag_filter: wgpu::FilterMode::Nearest,
                min_filter: wgpu::FilterMode::Nearest,
                mipmap_filter: wgpu::FilterMode::Nearest,
                ..Default::default()
            },
        )
        .unwrap();
        rt.set_ps_sampler(PS_SAMPLER_SLOT, Some(SAMPLER));

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

        rt.bind_shaders(Some(VS), None, Some(PS));
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

        rt.bind_shaders(Some(VS), None, Some(PS));
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

        rt.bind_shaders(Some(VS), None, Some(PS));
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
fn aerogpu_cmd_runtime_signature_driven_ps_two_constant_buffers_sm5() {
    // Ensure signature-driven runtime bindings support multiple constant buffers within the same
    // stage-scoped bind group (PS cb0 + cb1).
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
        const CB1: u32 = 6;
        const RTEX: u32 = 7;

        rt.create_shader_dxbc(VS, &build_vs_passthrough_pos_sm5_dxbc())
            .unwrap();
        rt.create_shader_dxbc(PS, &build_ps_two_constant_buffers_sm5_dxbc())
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

        let cb0_color: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
        rt.create_buffer(
            CB0,
            std::mem::size_of_val(&cb0_color) as u64,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(CB0, 0, bytemuck::bytes_of(&cb0_color))
            .unwrap();
        rt.set_ps_constant_buffer(0, Some(CB0));

        let cb1_color: [f32; 4] = [0.0, 1.0, 0.0, 1.0];
        rt.create_buffer(
            CB1,
            std::mem::size_of_val(&cb1_color) as u64,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(CB1, 0, bytemuck::bytes_of(&cb1_color))
            .unwrap();
        rt.set_ps_constant_buffer(1, Some(CB1));

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

        rt.bind_shaders(Some(VS), None, Some(PS));
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
fn aerogpu_cmd_runtime_signature_driven_too_small_constant_buffer_uses_dummy() {
    // If a bound constant buffer is smaller than the shader's minimum binding size, we should fall
    // back to the dummy uniform rather than failing bind group creation.
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
        // Read from cb0[1], requiring a 32-byte binding. We'll bind a 16-byte buffer to ensure it
        // is rejected and replaced with the dummy uniform.
        rt.create_shader_dxbc(PS, &build_ps_cbuffer_sm5_dxbc(0, 1))
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

        let cb0_color: [f32; 4] = [1.0, 0.0, 1.0, 1.0];
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
            1,
            1,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );

        let mut colors = [None; 8];
        colors[0] = Some(RTEX);
        rt.set_render_targets(&colors, None);

        rt.bind_shaders(Some(VS), None, Some(PS));
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
        assert_eq!(pixels, vec![0, 0, 0, 0], "dummy uniform fallback");
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

        rt.bind_shaders(Some(VS), None, Some(PS));
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

        rt.bind_shaders(Some(VS), None, Some(PS));
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
fn aerogpu_cmd_runtime_signature_driven_texture_load_binding_sm5() {
    // Ensure signature-driven runtime bindings work for texture load (`ld` / `textureLoad`) where
    // no sampler binding should be required.
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
        // Load the right texel (x=1) from a 2x1 texture.
        rt.create_shader_dxbc(PS, &build_ps_ld_t0_sm5_dxbc(1, 0, 0))
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
            1,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        );
        let tex_data: [[u8; 4]; 2] = [[255, 0, 0, 255], [0, 255, 0, 255]];
        rt.write_texture_rgba8(TEX, 2, 1, 2 * 4, bytemuck::bytes_of(&tex_data))
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

        rt.bind_shaders(Some(VS), None, Some(PS));
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
fn aerogpu_cmd_runtime_signature_driven_texture_load_binding_sm4() {
    // Same as `*_sm5`, but uses the checked-in SM4 DXBC fixture (`ps_ld.dxbc`).
    //
    // Also verifies that unbound textures fall back to the dummy texture for textureLoad.
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

        // `ps_ld.dxbc` includes an unused COLOR0 input in its signature. The runtime's pipeline
        // linker trims unused PS inputs automatically, so we can still pair it with the passthrough
        // VS. We use the POS3+COLOR input layout because `vs_passthrough.dxbc` consumes COLOR0.
        rt.create_shader_dxbc(VS, DXBC_VS_PASSTHROUGH).unwrap();
        rt.create_shader_dxbc(PS, DXBC_PS_LD).unwrap();
        rt.create_input_layout(IL, ILAY_POS3_COLOR).unwrap();

        let vertices: [VertexPos3Color4; 3] = [
            VertexPos3Color4 {
                pos: [-1.0, -1.0, 0.0],
                color: [0.0, 0.0, 0.0, 1.0],
            },
            VertexPos3Color4 {
                pos: [3.0, -1.0, 0.0],
                color: [0.0, 0.0, 0.0, 1.0],
            },
            VertexPos3Color4 {
                pos: [-1.0, 3.0, 0.0],
                color: [0.0, 0.0, 0.0, 1.0],
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
            1,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        );
        let tex_data: [[u8; 4]; 2] = [[255, 0, 0, 255], [0, 255, 0, 255]];
        rt.write_texture_rgba8(TEX, 2, 1, 2 * 4, bytemuck::bytes_of(&tex_data))
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

        rt.bind_shaders(Some(VS), None, Some(PS));
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
        assert_eq!(pixels, vec![255, 0, 0, 255], "bound texture");

        rt.set_ps_texture(0, None);
        rt.draw(3, 1, 0, 0).unwrap();
        rt.poll_wait();
        let pixels = rt.read_texture_rgba8(RTEX).await.unwrap();
        assert_eq!(pixels, vec![0, 0, 0, 255], "dummy texture fallback");
    });
}

#[test]
fn aerogpu_cmd_runtime_signature_driven_texture_sampler_slot1_binding_sm5() {
    // Ensure non-zero sampler slots bind correctly (t0 + s1).
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

        // Sample at out-of-range u so clamp vs repeat is visible.
        rt.create_shader_dxbc(VS, &build_vs_passthrough_pos_sm5_dxbc())
            .unwrap();
        rt.create_shader_dxbc(PS, &build_ps_sample_l_sm5_dxbc(1.1, 0.5, 0, 1))
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
        // 2x2 texture with left column red and right column green (both rows identical).
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

        rt.bind_shaders(Some(VS), None, Some(PS));
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

        // Explicit repeat sampler at s1 should wrap u=1.1 -> 0.1 and hit the left column.
        rt.create_sampler(
            SAMPLER,
            &wgpu::SamplerDescriptor {
                label: Some("aerogpu_cmd_runtime ps repeat sampler s1"),
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
        rt.set_ps_sampler(1, Some(SAMPLER));
        rt.draw(3, 1, 0, 0).unwrap();
        rt.poll_wait();
        let pixels = rt.read_texture_rgba8(RTEX).await.unwrap();
        assert_eq!(pixels, vec![255, 0, 0, 255], "repeat sampler at s1");
    });
}

#[test]
fn aerogpu_cmd_runtime_signature_driven_ps_cb0_texture_sampler_binding_sm5() {
    // Ensure signature-driven runtime bindings work when a *single* stage (PS) declares multiple
    // resource types in its stage-scoped bind group:
    // - cb0
    // - t0 + s0
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
        const TEX: u32 = 6;
        const RTEX: u32 = 7;

        rt.create_shader_dxbc(VS, &build_vs_passthrough_pos_sm5_dxbc())
            .unwrap();
        rt.create_shader_dxbc(PS, &build_ps_cbuffer0_and_sample_l_t0_s0_sm5_dxbc(0.0, 0.0))
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

        let cb0_color: [f32; 4] = [1.0, 0.0, 1.0, 1.0];
        rt.create_buffer(
            CB0,
            std::mem::size_of_val(&cb0_color) as u64,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        rt.write_buffer(CB0, 0, bytemuck::bytes_of(&cb0_color))
            .unwrap();
        rt.set_ps_constant_buffer(0, Some(CB0));

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

        rt.bind_shaders(Some(VS), None, Some(PS));
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
fn aerogpu_cmd_runtime_signature_driven_ps_two_texture_sampler_bindings_sm5() {
    // Ensure signature-driven runtime bindings support multiple texture/sampler slots in the same
    // stage-scoped bind group (t0+s0 and t1+s1).
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
        const TEX0: u32 = 5;
        const TEX1: u32 = 6;
        const RTEX: u32 = 7;

        rt.create_shader_dxbc(VS, &build_vs_passthrough_pos_sm5_dxbc())
            .unwrap();
        rt.create_shader_dxbc(PS, &build_ps_two_sample_l_sm5_dxbc(0.0, 0.0))
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
            TEX0,
            2,
            2,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        );
        let red_px: [u8; 4] = [255, 0, 0, 255];
        let tex0_data = [
            red_px, red_px, //
            red_px, red_px, //
        ];
        rt.write_texture_rgba8(TEX0, 2, 2, 2 * 4, bytemuck::bytes_of(&tex0_data))
            .unwrap();
        rt.set_ps_texture(0, Some(TEX0));

        rt.create_texture2d(
            TEX1,
            2,
            2,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        );
        let green_px: [u8; 4] = [0, 255, 0, 255];
        let tex1_data = [
            green_px, green_px, //
            green_px, green_px, //
        ];
        rt.write_texture_rgba8(TEX1, 2, 2, 2 * 4, bytemuck::bytes_of(&tex1_data))
            .unwrap();
        rt.set_ps_texture(1, Some(TEX1));

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

        rt.bind_shaders(Some(VS), None, Some(PS));
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
        assert_eq!(pixels, vec![0, 255, 0, 255], "t1+s1 sample wins");
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

        rt.bind_shaders(Some(VS), None, Some(PS));
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

        rt.bind_shaders(Some(VS), None, Some(PS));
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
fn aerogpu_cmd_runtime_signature_driven_vs_texture_load_binding_sm5() {
    // Ensure signature-driven runtime bindings work for vertex-stage texture load (`ld` /
    // `textureLoad`) where no sampler binding should be required.
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

        rt.create_shader_dxbc(VS, &build_vs_ld_t0_to_color1_sm5_dxbc(1, 0, 0))
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
            1,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        );
        let tex_data: [[u8; 4]; 2] = [[255, 0, 0, 255], [0, 255, 0, 255]];
        rt.write_texture_rgba8(TEX, 2, 1, 2 * 4, bytemuck::bytes_of(&tex_data))
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

        rt.bind_shaders(Some(VS), None, Some(PS));
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
fn aerogpu_cmd_runtime_signature_driven_vs_two_texture_sampler_bindings_sm5() {
    // Ensure signature-driven runtime bindings support multiple texture+sampler slots in the VS
    // stage-scoped bind group (t0+s0 and t1+s1).
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
        const TEX0: u32 = 5;
        const TEX1: u32 = 6;
        const RTEX: u32 = 7;

        rt.create_shader_dxbc(VS, &build_vs_two_sample_l_sm5_dxbc(0.0, 0.0))
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
            TEX0,
            2,
            2,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        );
        let red_px: [u8; 4] = [255, 0, 0, 255];
        let tex0_data = [
            red_px, red_px, //
            red_px, red_px, //
        ];
        rt.write_texture_rgba8(TEX0, 2, 2, 2 * 4, bytemuck::bytes_of(&tex0_data))
            .unwrap();
        rt.set_vs_texture(0, Some(TEX0));

        rt.create_texture2d(
            TEX1,
            2,
            2,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        );
        let green_px: [u8; 4] = [0, 255, 0, 255];
        let tex1_data = [
            green_px, green_px, //
            green_px, green_px, //
        ];
        rt.write_texture_rgba8(TEX1, 2, 2, 2 * 4, bytemuck::bytes_of(&tex1_data))
            .unwrap();
        rt.set_vs_texture(1, Some(TEX1));

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

        rt.bind_shaders(Some(VS), None, Some(PS));
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
        assert_eq!(pixels, vec![0, 255, 0, 255], "t1+s1 sample wins");
    });
}

#[test]
fn aerogpu_cmd_runtime_signature_driven_vs_cb0_texture_sampler_binding_sm5() {
    // Ensure signature-driven runtime bindings work when a *single* stage (VS) declares multiple
    // resource types in its stage-scoped bind group:
    // - cb0
    // - t0 + s0
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
        const TEX: u32 = 6;
        const RTEX: u32 = 7;

        rt.create_shader_dxbc(VS, &build_vs_matrix_sample_t0_s0_sm5_dxbc(0.25, 0.25))
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

        rt.bind_shaders(Some(VS), None, Some(PS));
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

        rt.bind_shaders(Some(VS), None, Some(PS));
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
