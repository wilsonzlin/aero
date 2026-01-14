use pretty_assertions::assert_eq;

use std::collections::HashMap;

use aero_dxbc::{
    parse_ctab_chunk, parse_rdef_chunk, parse_signature_chunk, test_utils as dxbc_test_utils,
    FourCC as DxbcFourCC,
};

use crate::shader_limits::{
    MAX_D3D9_SHADER_BLOB_BYTES, MAX_D3D9_SHADER_BYTECODE_BYTES,
    MAX_D3D9_SHADER_CONTROL_FLOW_NESTING,
};
use crate::sm3::decode::TextureType;
use crate::{dxbc, shader, shader_translate, sm3, software, state};

fn enc_reg_type(ty: u8) -> u32 {
    let low = (ty & 0x7) as u32;
    let high = (ty & 0x18) as u32;
    (low << 28) | (high << 8)
}

fn enc_src(reg_type: u8, reg_num: u16, swizzle: u8) -> u32 {
    enc_reg_type(reg_type) | (reg_num as u32) | ((swizzle as u32) << 16)
}

fn enc_src_mod(reg_type: u8, reg_num: u16, swizzle: u8, modifier: u8) -> u32 {
    enc_reg_type(reg_type) | (reg_num as u32) | ((swizzle as u32) << 16) | ((modifier as u32) << 24)
}

fn enc_dst(reg_type: u8, reg_num: u16, mask: u8) -> u32 {
    enc_reg_type(reg_type) | (reg_num as u32) | ((mask as u32) << 16)
}

fn enc_inst(opcode: u16, params: &[u32]) -> Vec<u32> {
    // SM2/SM3 encodes the *total* instruction length in tokens (including the opcode token) in
    // bits 24..27.
    let token = (opcode as u32) | (((params.len() as u32) + 1) << 24);
    let mut v = vec![token];
    v.extend_from_slice(params);
    v
}
fn enc_inst_with_extra(opcode: u16, extra: u32, params: &[u32]) -> Vec<u32> {
    let token = (opcode as u32) | (((params.len() as u32) + 1) << 24) | extra;
    let mut v = vec![token];
    v.extend_from_slice(params);
    v
}

fn enc_inst_operand_count_len(opcode: u16, params: &[u32]) -> Vec<u32> {
    // Some historical shader blobs encode opcode token "length" as the number of operand tokens
    // (excluding the opcode token itself). `shader_translate` is expected to accept both
    // encodings.
    let token = (opcode as u32) | ((params.len() as u32) << 24);
    let mut v = vec![token];
    v.extend_from_slice(params);
    v
}

fn enc_inst_with_extra_operand_count_len(opcode: u16, extra: u32, params: &[u32]) -> Vec<u32> {
    let token = (opcode as u32) | ((params.len() as u32) << 24) | extra;
    let mut v = vec![token];
    v.extend_from_slice(params);
    v
}

// Some tests build SM3 shaders explicitly (vs_3_0/ps_3_0). These helpers are currently identical
// to the generic encoders above; they exist to make intent explicit at call sites.
#[allow(dead_code)]
fn enc_inst_sm3(opcode: u16, params: &[u32]) -> Vec<u32> {
    enc_inst(opcode, params)
}
#[allow(dead_code)]
fn enc_inst_with_extra_sm3(opcode: u16, extra: u32, params: &[u32]) -> Vec<u32> {
    enc_inst_with_extra(opcode, extra, params)
}

fn assemble_vs_passthrough() -> Vec<u32> {
    // vs_2_0
    let mut out = vec![0xFFFE0200];
    // mov oPos, v0
    out.extend(enc_inst(0x0001, &[enc_dst(4, 0, 0xF), enc_src(1, 0, 0xE4)]));
    // mov oT0, v1
    out.extend(enc_inst(0x0001, &[enc_dst(6, 0, 0xF), enc_src(1, 1, 0xE4)]));
    // mov oD0, v2
    out.extend(enc_inst(0x0001, &[enc_dst(5, 0, 0xF), enc_src(1, 2, 0xE4)]));
    // end
    out.push(0x0000FFFF);
    out
}

fn assemble_vs_passthrough_sm3_decoder() -> Vec<u32> {
    // `assemble_vs_passthrough` already uses SM2/SM3's real instruction length encoding.
    assemble_vs_passthrough()
}

fn assemble_vs_passthrough_with_dcl_sm3_decoder() -> Vec<u32> {
    // vs_2_0 with DCL semantics so the IR builder can remap input registers to canonical
    // WGSL @location(n) indices (`ShaderIr.uses_semantic_locations = true`).
    let mut out = vec![0xFFFE0200];

    // dcl_position v0
    out.extend(enc_inst_with_extra_sm3(0x001F, 0, &[enc_dst(1, 0, 0xF)]));
    // dcl_texcoord0 v1.xy
    out.extend(enc_inst_with_extra_sm3(
        0x001F,
        5u32 << 16,
        &[enc_dst(1, 1, 0x3)],
    ));
    // dcl_color0 v2
    out.extend(enc_inst_with_extra_sm3(
        0x001F,
        10u32 << 16,
        &[enc_dst(1, 2, 0xF)],
    ));

    // mov oPos, v0
    out.extend(enc_inst(0x0001, &[enc_dst(4, 0, 0xF), enc_src(1, 0, 0xE4)]));
    // mov oT0, v1
    out.extend(enc_inst(0x0001, &[enc_dst(6, 0, 0xF), enc_src(1, 1, 0xE4)]));
    // mov oD0, v2
    out.extend(enc_inst(0x0001, &[enc_dst(5, 0, 0xF), enc_src(1, 2, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_vs_passthrough_with_texcoord8_dcl_sm3_decoder() -> Vec<u32> {
    // vs_2_0 with a TEXCOORD8 input semantic. This is outside the fixed StandardLocationMap range
    // and exercises the adaptive semantic→location allocator.
    let mut out = vec![0xFFFE0200];
    // dcl_position v0
    out.extend(enc_inst_with_extra(0x001F, 0, &[enc_dst(1, 0, 0xF)]));
    // dcl_texcoord0 v1
    out.extend(enc_inst_with_extra(
        0x001F,
        5u32 << 16,
        &[enc_dst(1, 1, 0xF)],
    ));
    // dcl_texcoord8 v2
    out.extend(enc_inst_with_extra(
        0x001F,
        (5u32 << 16) | (8u32 << 20),
        &[enc_dst(1, 2, 0xF)],
    ));

    // mov oPos, v0
    out.extend(enc_inst(0x0001, &[enc_dst(4, 0, 0xF), enc_src(1, 0, 0xE4)]));
    // mov oT0, v1
    out.extend(enc_inst(0x0001, &[enc_dst(6, 0, 0xF), enc_src(1, 1, 0xE4)]));
    // mov oT1, v2 (ensure TEXCOORD8 input is actually used)
    out.extend(enc_inst(0x0001, &[enc_dst(6, 1, 0xF), enc_src(1, 2, 0xE4)]));

    out.push(0x0000FFFF);
    out
}

fn assemble_vs_passthrough_with_texcoord8_and_unused_normal_dcl_sm3_decoder() -> Vec<u32> {
    // vs_2_0 with an unused NORMAL0 declaration. This exercises the host-side semantic location
    // reflection, which should still report the canonical location for declared-but-unused inputs.
    //
    // Layout:
    //   v0: POSITION0 (used)
    //   v1: TEXCOORD8 (used)
    //   v2: NORMAL0 (unused)
    let mut out = vec![0xFFFE0200];
    // dcl_position v0
    out.extend(enc_inst_with_extra(0x001F, 0, &[enc_dst(1, 0, 0xF)]));
    // dcl_texcoord8 v1
    out.extend(enc_inst_with_extra(
        0x001F,
        (5u32 << 16) | (8u32 << 20),
        &[enc_dst(1, 1, 0xF)],
    ));
    // dcl_normal v2
    out.extend(enc_inst_with_extra(
        0x001F,
        3u32 << 16,
        &[enc_dst(1, 2, 0xF)],
    ));

    // mov oPos, v0
    out.extend(enc_inst(0x0001, &[enc_dst(4, 0, 0xF), enc_src(1, 0, 0xE4)]));
    // mov oT0, v1 (ensure TEXCOORD8 input is actually used)
    out.extend(enc_inst(0x0001, &[enc_dst(6, 0, 0xF), enc_src(1, 1, 0xE4)]));

    out.push(0x0000FFFF);
    out
}
fn assemble_ps2_mov_oc0_t0_sm3_decoder() -> Vec<u32> {
    // ps_2_0
    let mut out = vec![0xFFFF0200];
    // mov oC0, t0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(3, 0, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_vs3_generic_output_texcoord3_constant_sm3_decoder() -> Vec<u32> {
    // vs_3_0: outputs TEXCOORD3 via generic output register o0 so we can exercise the
    // semantic-based varying location mapping.
    let mut out = vec![0xFFFE0300];

    // dcl_position v0
    out.extend(enc_inst_with_extra_sm3(0x001F, 0, &[enc_dst(1, 0, 0xF)]));
    // dcl_position oPos
    out.extend(enc_inst_with_extra_sm3(0x001F, 0, &[enc_dst(4, 0, 0xF)]));
    // dcl_texcoord3 o0
    out.extend(enc_inst_with_extra_sm3(
        0x001F,
        (5u32 << 16) | (3u32 << 20),
        &[enc_dst(6, 0, 0xF)],
    ));

    // def c0, 0.25, 0.5, 0.0, 1.0
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            0x3E80_0000,
            0x3F00_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));

    // mov oPos, v0
    out.extend(enc_inst(0x0001, &[enc_dst(4, 0, 0xF), enc_src(1, 0, 0xE4)]));
    // mov o0, c0
    out.extend(enc_inst(0x0001, &[enc_dst(6, 0, 0xF), enc_src(2, 0, 0xE4)]));

    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_input_texcoord3_passthrough_sm3_decoder() -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];

    // dcl_texcoord3 v0
    out.extend(enc_inst_with_extra_sm3(
        0x001F,
        (5u32 << 16) | (3u32 << 20),
        &[enc_dst(1, 0, 0xF)],
    ));
    // mov oC0, v0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(1, 0, 0xE4)]));

    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_dcl_position_quadrants_sm3_decoder() -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];
    // dcl_position v0
    out.extend(enc_inst_with_extra_sm3(0x001F, 0, &[enc_dst(1, 0, 0xF)]));
    // def c0, 4.0, 4.0, 4.0, 4.0
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            0x4080_0000,
            0x4080_0000,
            0x4080_0000,
            0x4080_0000,
        ],
    ));
    // def c1, 1.0, 0.0, 0.0, 1.0 (red)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 1, 0xF),
            0x3F80_0000,
            0x0000_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));
    // def c2, 0.0, 1.0, 0.0, 1.0 (green)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 2, 0xF),
            0x0000_0000,
            0x3F80_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));
    // def c3, 0.0, 0.0, 1.0, 1.0 (blue)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 3, 0xF),
            0x0000_0000,
            0x0000_0000,
            0x3F80_0000,
            0x3F80_0000,
        ],
    ));
    // def c4, 1.0, 1.0, 1.0, 1.0 (white)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 4, 0xF),
            0x3F80_0000,
            0x3F80_0000,
            0x3F80_0000,
            0x3F80_0000,
        ],
    ));
    // add r0, v0.xxxx, -c0
    out.extend(enc_inst(
        0x0002,
        &[
            enc_dst(0, 0, 0xF),
            enc_src(1, 0, 0x00),
            enc_src_mod(2, 0, 0xE4, 1),
        ],
    ));
    // add r1, v0.yyyy, -c0
    out.extend(enc_inst(
        0x0002,
        &[
            enc_dst(0, 1, 0xF),
            enc_src(1, 0, 0x55),
            enc_src_mod(2, 0, 0xE4, 1),
        ],
    ));
    // cmp r2, r0, c2, c1
    out.extend(enc_inst(
        0x0058,
        &[
            enc_dst(0, 2, 0xF),
            enc_src(0, 0, 0xE4),
            enc_src(2, 2, 0xE4),
            enc_src(2, 1, 0xE4),
        ],
    ));
    // cmp r3, r0, c4, c3
    out.extend(enc_inst(
        0x0058,
        &[
            enc_dst(0, 3, 0xF),
            enc_src(0, 0, 0xE4),
            enc_src(2, 4, 0xE4),
            enc_src(2, 3, 0xE4),
        ],
    ));
    // cmp oC0, r1, r3, r2
    out.extend(enc_inst(
        0x0058,
        &[
            enc_dst(8, 0, 0xF),
            enc_src(0, 1, 0xE4),
            enc_src(0, 3, 0xE4),
            enc_src(0, 2, 0xE4),
        ],
    ));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_vpos_quadrants_sm3_decoder() -> Vec<u32> {
    // ps_3_0: classify pixels into quadrants based on `vPos` (`misc0`) and output solid colors:
    // top-left red, top-right green, bottom-left blue, bottom-right white.
    let mut out = vec![0xFFFF0300];
    // def c0, 4.0, 4.0, 4.0, 4.0
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            0x4080_0000,
            0x4080_0000,
            0x4080_0000,
            0x4080_0000,
        ],
    ));
    // def c1, 1.0, 0.0, 0.0, 1.0 (red)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 1, 0xF),
            0x3F80_0000,
            0x0000_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));
    // def c2, 0.0, 1.0, 0.0, 1.0 (green)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 2, 0xF),
            0x0000_0000,
            0x3F80_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));
    // def c3, 0.0, 0.0, 1.0, 1.0 (blue)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 3, 0xF),
            0x0000_0000,
            0x0000_0000,
            0x3F80_0000,
            0x3F80_0000,
        ],
    ));
    // def c4, 1.0, 1.0, 1.0, 1.0 (white)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 4, 0xF),
            0x3F80_0000,
            0x3F80_0000,
            0x3F80_0000,
            0x3F80_0000,
        ],
    ));
    // add r0, misc0.xxxx, -c0
    out.extend(enc_inst(
        0x0002,
        &[
            enc_dst(0, 0, 0xF),
            enc_src(17, 0, 0x00),
            enc_src_mod(2, 0, 0xE4, 1),
        ],
    ));
    // add r1, misc0.yyyy, -c0
    out.extend(enc_inst(
        0x0002,
        &[
            enc_dst(0, 1, 0xF),
            enc_src(17, 0, 0x55),
            enc_src_mod(2, 0, 0xE4, 1),
        ],
    ));
    // cmp r2, r0, c2, c1
    out.extend(enc_inst(
        0x0058,
        &[
            enc_dst(0, 2, 0xF),
            enc_src(0, 0, 0xE4),
            enc_src(2, 2, 0xE4),
            enc_src(2, 1, 0xE4),
        ],
    ));
    // cmp r3, r0, c4, c3
    out.extend(enc_inst(
        0x0058,
        &[
            enc_dst(0, 3, 0xF),
            enc_src(0, 0, 0xE4),
            enc_src(2, 4, 0xE4),
            enc_src(2, 3, 0xE4),
        ],
    ));
    // cmp oC0, r1, r3, r2
    out.extend(enc_inst(
        0x0058,
        &[
            enc_dst(8, 0, 0xF),
            enc_src(0, 1, 0xE4),
            enc_src(0, 3, 0xE4),
            enc_src(0, 2, 0xE4),
        ],
    ));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_vface_sm3_decoder() -> Vec<u32> {
    // ps_3_0: outputs red for front-facing pixels and blue for back-facing pixels via `vFace`
    // (misc1).
    let mut out = vec![0xFFFF0300];
    // def c0, 1.0, 0.0, 0.0, 1.0 (red)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            0x3F80_0000,
            0x0000_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));
    // def c1, 0.0, 0.0, 1.0, 1.0 (blue)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 1, 0xF),
            0x0000_0000,
            0x0000_0000,
            0x3F80_0000,
            0x3F80_0000,
        ],
    ));
    // cmp oC0, misc1, c0, c1  (misc1.x >= 0 ? red : blue)
    out.extend(enc_inst(
        0x0058,
        &[
            enc_dst(8, 0, 0xF),
            enc_src(17, 1, 0xE4), // misc1 (vFace)
            enc_src(2, 0, 0xE4),  // red
            enc_src(2, 1, 0xE4),  // blue
        ],
    ));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps_texture_modulate() -> Vec<u32> {
    // ps_2_0
    let mut out = vec![0xFFFF0200];
    // texld r0, t0, s0
    out.extend(enc_inst(
        0x0042,
        &[
            enc_dst(0, 0, 0xF),   // r0
            enc_src(3, 0, 0xE4),  // t0
            enc_src(10, 0, 0xE4), // s0
        ],
    ));
    // mul r0, r0, v0 (modulate by color)
    out.extend(enc_inst(
        0x0005,
        &[
            enc_dst(0, 0, 0xF),
            enc_src(0, 0, 0xE4),
            enc_src(1, 0, 0xE4), // v0 treated as input (color)
        ],
    ));
    // mov oC0, r0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps_color_passthrough() -> Vec<u32> {
    // ps_2_0
    let mut out = vec![0xFFFF0200];
    // mov oC0, v0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(1, 0, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps_math_ops() -> Vec<u32> {
    // ps_2_0
    let mut out = vec![0xFFFF0200];

    // mov r0, c0
    out.extend(enc_inst(0x0001, &[enc_dst(0, 0, 0xF), enc_src(2, 0, 0xE4)]));
    // min r0, r0, c1
    out.extend(enc_inst(
        0x000A,
        &[enc_dst(0, 0, 0xF), enc_src(0, 0, 0xE4), enc_src(2, 1, 0xE4)],
    ));
    // max r0, r0, c2
    out.extend(enc_inst(
        0x000B,
        &[enc_dst(0, 0, 0xF), enc_src(0, 0, 0xE4), enc_src(2, 2, 0xE4)],
    ));
    // rcp r1, c3
    out.extend(enc_inst(0x0006, &[enc_dst(0, 1, 0xF), enc_src(2, 3, 0xE4)]));
    // rsq r2, c4
    out.extend(enc_inst(0x0007, &[enc_dst(0, 2, 0xF), enc_src(2, 4, 0xE4)]));
    // frc r3, c5
    out.extend(enc_inst(0x0013, &[enc_dst(0, 3, 0xF), enc_src(2, 5, 0xE4)]));
    // exp r7, c0
    out.extend(enc_inst(0x000E, &[enc_dst(0, 7, 0xF), enc_src(2, 0, 0xE4)]));
    // log r8, c1
    out.extend(enc_inst(0x000F, &[enc_dst(0, 8, 0xF), enc_src(2, 1, 0xE4)]));
    // pow r9, c0, c1
    out.extend(enc_inst(
        0x0020,
        &[enc_dst(0, 9, 0xF), enc_src(2, 0, 0xE4), enc_src(2, 1, 0xE4)],
    ));
    // slt r4, c6, c7
    out.extend(enc_inst(
        0x000C,
        &[enc_dst(0, 4, 0xF), enc_src(2, 6, 0xE4), enc_src(2, 7, 0xE4)],
    ));
    // sge r5, c8, c9
    out.extend(enc_inst(
        0x000D,
        &[enc_dst(0, 5, 0xF), enc_src(2, 8, 0xE4), enc_src(2, 9, 0xE4)],
    ));
    // cmp r6, c10, c11, c12
    out.extend(enc_inst(
        0x0058,
        &[
            enc_dst(0, 6, 0xF),
            enc_src(2, 10, 0xE4),
            enc_src(2, 11, 0xE4),
            enc_src(2, 12, 0xE4),
        ],
    ));
    // dp2add r10, c0, c1, c2
    out.extend(enc_inst(
        0x0059,
        &[
            enc_dst(0, 10, 0xF),
            enc_src(2, 0, 0xE4),
            enc_src(2, 1, 0xE4),
            enc_src(2, 2, 0xE4),
        ],
    ));
    // mov oC0, r0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));

    out.push(0x0000FFFF);
    out
}

fn assemble_ps_with_unknown_opcode() -> Vec<u32> {
    // ps_2_0
    let mut out = vec![0xFFFF0200];
    // mov oC0, c0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(2, 0, 0xE4)]));
    // Unknown opcode with 0 operands. The legacy translator skips this, while the SM3 decoder
    // errors out with "unsupported opcode".
    out.extend(enc_inst(0x1234, &[]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps_with_unknown_opcode_and_derivatives() -> Vec<u32> {
    // ps_2_0
    let mut out = vec![0xFFFF0200];
    // dsx r0, t0
    out.extend(enc_inst(0x0056, &[enc_dst(0, 0, 0xF), enc_src(3, 0, 0xE4)]));
    // dsy r1, t0
    out.extend(enc_inst(0x0057, &[enc_dst(0, 1, 0xF), enc_src(3, 0, 0xE4)]));
    // add r0, r0, r1
    out.extend(enc_inst(
        0x0002,
        &[enc_dst(0, 0, 0xF), enc_src(0, 0, 0xE4), enc_src(0, 1, 0xE4)],
    ));
    // Unknown opcode with 0 operands. The legacy translator skips this, while the SM3 decoder
    // errors out with "unsupported opcode".
    out.extend(enc_inst(0x1234, &[]));
    // mov oC0, r0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_nonuniform_if_dsx() -> Vec<u32> {
    // ps_3_0
    //
    // Uses a non-uniform branch condition (`v0`) around `dsx` to exercise the legacy translator's
    // uniform-control-flow workaround for WGSL derivative ops.
    let mut out = vec![0xFFFF0300];
    // if v0
    out.extend(enc_inst(0x0028, &[enc_src(1, 0, 0xE4)]));
    // dsx r0, v0
    out.extend(enc_inst(0x0056, &[enc_dst(0, 0, 0xF), enc_src(1, 0, 0xE4)]));
    // endif
    out.extend(enc_inst(0x002B, &[]));
    // mov oC0, r0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_nonuniform_if_dsy() -> Vec<u32> {
    // ps_3_0
    //
    // Uses a non-uniform branch condition (`v0`) around `dsy` to exercise the legacy translator's
    // uniform-control-flow workaround for WGSL derivative ops.
    let mut out = vec![0xFFFF0300];
    // if v0
    out.extend(enc_inst(0x0028, &[enc_src(1, 0, 0xE4)]));
    // dsy r0, v0
    out.extend(enc_inst(0x0057, &[enc_dst(0, 0, 0xF), enc_src(1, 0, 0xE4)]));
    // endif
    out.extend(enc_inst(0x002B, &[]));
    // mov oC0, r0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_nonuniform_ifc_dsx() -> Vec<u32> {
    // ps_3_0
    //
    // Uses `ifc` (compare) around `dsx` to ensure the uniform-control-flow rewrite handles `ifc`
    // conditions in addition to simple `if`.
    let mut out = vec![0xFFFF0300];
    // ifc v0, c0 (cmp_code=4 => !=)
    out.extend(enc_inst_with_extra(
        0x0029,
        4u32 << 16,
        &[enc_src(1, 0, 0xE4), enc_src(2, 0, 0xE4)],
    ));
    // dsx r0, v0
    out.extend(enc_inst(0x0056, &[enc_dst(0, 0, 0xF), enc_src(1, 0, 0xE4)]));
    // endif
    out.extend(enc_inst(0x002B, &[]));
    // mov oC0, r0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_nonuniform_if_dsx_then_mov() -> Vec<u32> {
    // ps_3_0
    //
    // Non-uniform `if v0 { dsx; mov }` to ensure the legacy translator can hoist derivatives out of
    // multi-statement `if` blocks.
    let mut out = vec![0xFFFF0300];
    // if v0
    out.extend(enc_inst(0x0028, &[enc_src(1, 0, 0xE4)]));
    // dsx r0, v0
    out.extend(enc_inst(0x0056, &[enc_dst(0, 0, 0xF), enc_src(1, 0, 0xE4)]));
    // mov r1, r0
    out.extend(enc_inst(0x0001, &[enc_dst(0, 1, 0xF), enc_src(0, 0, 0xE4)]));
    // endif
    out.extend(enc_inst(0x002B, &[]));
    // mov oC0, r1
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 1, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_nonuniform_if_mov_then_dsx() -> Vec<u32> {
    // ps_3_0
    //
    // Non-uniform `if v0 { mov; dsx }` to ensure we can avoid invalid control flow when the
    // derivative op is not the first instruction in the branch.
    let mut out = vec![0xFFFF0300];
    // if v0
    out.extend(enc_inst(0x0028, &[enc_src(1, 0, 0xE4)]));
    // mov r0, c0
    out.extend(enc_inst(0x0001, &[enc_dst(0, 0, 0xF), enc_src(2, 0, 0xE4)]));
    // dsx r1, v0
    out.extend(enc_inst(0x0056, &[enc_dst(0, 1, 0xF), enc_src(1, 0, 0xE4)]));
    // endif
    out.extend(enc_inst(0x002B, &[]));
    // mov oC0, r1
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 1, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_nonuniform_if_nested_dsx() -> Vec<u32> {
    // ps_3_0
    //
    // Non-uniform nested `if v0 { if v0 { dsx } }` to ensure the legacy translator can hoist
    // derivatives out of nested control flow (WGSL requires uniform control flow for `dpdx`).
    let mut out = vec![0xFFFF0300];
    // if v0
    out.extend(enc_inst(0x0028, &[enc_src(1, 0, 0xE4)]));
    // if v0
    out.extend(enc_inst(0x0028, &[enc_src(1, 0, 0xE4)]));
    // dsx r0, v0
    out.extend(enc_inst(0x0056, &[enc_dst(0, 0, 0xF), enc_src(1, 0, 0xE4)]));
    // endif (inner)
    out.extend(enc_inst(0x002B, &[]));
    // endif (outer)
    out.extend(enc_inst(0x002B, &[]));
    // mov oC0, r0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_nonuniform_if_else_dsx() -> Vec<u32> {
    // ps_3_0
    //
    // Non-uniform `if v0 { mov } else { dsx }` to ensure the legacy translator hoists derivatives
    // out of `else` blocks.
    let mut out = vec![0xFFFF0300];
    // if v0
    out.extend(enc_inst(0x0028, &[enc_src(1, 0, 0xE4)]));
    // mov r0, c0
    out.extend(enc_inst(0x0001, &[enc_dst(0, 0, 0xF), enc_src(2, 0, 0xE4)]));
    // else
    out.extend(enc_inst(0x002A, &[]));
    // dsx r0, v0
    out.extend(enc_inst(0x0056, &[enc_dst(0, 0, 0xF), enc_src(1, 0, 0xE4)]));
    // endif
    out.extend(enc_inst(0x002B, &[]));
    // mov oC0, r0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_nonuniform_if_else_dsx_then_mov() -> Vec<u32> {
    // ps_3_0
    //
    // Non-uniform `if v0 { mov } else { dsx; mov }` to ensure we can hoist derivatives out of
    // multi-statement else blocks.
    let mut out = vec![0xFFFF0300];
    // if v0
    out.extend(enc_inst(0x0028, &[enc_src(1, 0, 0xE4)]));
    // mov r0, c0
    out.extend(enc_inst(0x0001, &[enc_dst(0, 0, 0xF), enc_src(2, 0, 0xE4)]));
    // else
    out.extend(enc_inst(0x002A, &[]));
    // dsx r0, v0
    out.extend(enc_inst(0x0056, &[enc_dst(0, 0, 0xF), enc_src(1, 0, 0xE4)]));
    // mov r1, r0
    out.extend(enc_inst(0x0001, &[enc_dst(0, 1, 0xF), enc_src(0, 0, 0xE4)]));
    // endif
    out.extend(enc_inst(0x002B, &[]));
    // mov oC0, r1
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 1, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_nonuniform_if_else_mov_then_dsx() -> Vec<u32> {
    // ps_3_0
    //
    // Non-uniform `if v0 { mov } else { mov; dsx }` to ensure we can hoist derivatives out of
    // larger else blocks where the derivative op is not the first instruction.
    let mut out = vec![0xFFFF0300];
    // if v0
    out.extend(enc_inst(0x0028, &[enc_src(1, 0, 0xE4)]));
    // mov r0, c0
    out.extend(enc_inst(0x0001, &[enc_dst(0, 0, 0xF), enc_src(2, 0, 0xE4)]));
    // else
    out.extend(enc_inst(0x002A, &[]));
    // mov r0, c1
    out.extend(enc_inst(0x0001, &[enc_dst(0, 0, 0xF), enc_src(2, 1, 0xE4)]));
    // dsx r1, v0
    out.extend(enc_inst(0x0056, &[enc_dst(0, 1, 0xF), enc_src(1, 0, 0xE4)]));
    // endif
    out.extend(enc_inst(0x002B, &[]));
    // mov oC0, r1
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 1, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps2_nonuniform_if_texld() -> Vec<u32> {
    // ps_2_0
    //
    // Uses a non-uniform branch condition (`v0`) around `texld` (implicit derivatives) to
    // exercise the legacy translator's uniform-control-flow workaround for `textureSample`.
    let mut out = vec![0xFFFF0200];
    // if v0
    out.extend(enc_inst(0x0028, &[enc_src(1, 0, 0xE4)]));
    // texld r0, t0, s0
    out.extend(enc_inst(
        0x0042,
        &[
            enc_dst(0, 0, 0xF),   // r0
            enc_src(3, 0, 0xE4),  // t0
            enc_src(10, 0, 0xE4), // s0
        ],
    ));
    // endif
    out.extend(enc_inst(0x002B, &[]));
    // mov oC0, r0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps2_nonuniform_if_mov_then_texld() -> Vec<u32> {
    // ps_2_0
    //
    // Non-uniform `if v0 { mov; texld }` to ensure we can avoid invalid control flow when implicit
    // derivative texture sampling is not the first instruction in the branch.
    let mut out = vec![0xFFFF0200];
    // if v0
    out.extend(enc_inst(0x0028, &[enc_src(1, 0, 0xE4)]));
    // mov r0, c0
    out.extend(enc_inst(0x0001, &[enc_dst(0, 0, 0xF), enc_src(2, 0, 0xE4)]));
    // texld r1, t0, s0
    out.extend(enc_inst(
        0x0042,
        &[
            enc_dst(0, 1, 0xF),   // r1
            enc_src(3, 0, 0xE4),  // t0
            enc_src(10, 0, 0xE4), // s0
        ],
    ));
    // endif
    out.extend(enc_inst(0x002B, &[]));
    // mov oC0, r1
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 1, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps2_nonuniform_if_nested_texld() -> Vec<u32> {
    // ps_2_0
    //
    // Non-uniform nested `if v0 { if v0 { texld } }` to ensure the legacy translator hoists
    // implicit-derivative texture sampling out of nested control flow (WGSL requires uniform
    // control flow for `textureSample`).
    let mut out = vec![0xFFFF0200];
    // if v0
    out.extend(enc_inst(0x0028, &[enc_src(1, 0, 0xE4)]));
    // if v0
    out.extend(enc_inst(0x0028, &[enc_src(1, 0, 0xE4)]));
    // texld r0, t0, s0
    out.extend(enc_inst(
        0x0042,
        &[
            enc_dst(0, 0, 0xF),   // r0
            enc_src(3, 0, 0xE4),  // t0
            enc_src(10, 0, 0xE4), // s0
        ],
    ));
    // endif (inner)
    out.extend(enc_inst(0x002B, &[]));
    // endif (outer)
    out.extend(enc_inst(0x002B, &[]));
    // mov oC0, r0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps2_nonuniform_if_else_texld() -> Vec<u32> {
    // ps_2_0
    //
    // Non-uniform `if v0 { mov } else { texld }` to ensure the legacy translator hoists implicit
    // derivative texture sampling out of `else` blocks.
    let mut out = vec![0xFFFF0200];
    // if v0
    out.extend(enc_inst(0x0028, &[enc_src(1, 0, 0xE4)]));
    // mov r0, c0
    out.extend(enc_inst(0x0001, &[enc_dst(0, 0, 0xF), enc_src(2, 0, 0xE4)]));
    // else
    out.extend(enc_inst(0x002A, &[]));
    // texld r0, t0, s0
    out.extend(enc_inst(
        0x0042,
        &[
            enc_dst(0, 0, 0xF),   // r0
            enc_src(3, 0, 0xE4),  // t0
            enc_src(10, 0, 0xE4), // s0
        ],
    ));
    // endif
    out.extend(enc_inst(0x002B, &[]));
    // mov oC0, r0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps2_nonuniform_if_else_texld_then_mov() -> Vec<u32> {
    // ps_2_0
    //
    // Non-uniform `if v0 { mov } else { texld; mov }` to ensure we can hoist implicit derivative
    // texture sampling out of multi-statement else blocks.
    let mut out = vec![0xFFFF0200];
    // if v0
    out.extend(enc_inst(0x0028, &[enc_src(1, 0, 0xE4)]));
    // mov r0, c0
    out.extend(enc_inst(0x0001, &[enc_dst(0, 0, 0xF), enc_src(2, 0, 0xE4)]));
    // else
    out.extend(enc_inst(0x002A, &[]));
    // texld r0, t0, s0
    out.extend(enc_inst(
        0x0042,
        &[
            enc_dst(0, 0, 0xF),   // r0
            enc_src(3, 0, 0xE4),  // t0
            enc_src(10, 0, 0xE4), // s0
        ],
    ));
    // mov r1, r0
    out.extend(enc_inst(0x0001, &[enc_dst(0, 1, 0xF), enc_src(0, 0, 0xE4)]));
    // endif
    out.extend(enc_inst(0x002B, &[]));
    // mov oC0, r1
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 1, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps2_nonuniform_if_else_mov_then_texld() -> Vec<u32> {
    // ps_2_0
    //
    // Non-uniform `if v0 { mov } else { mov; texld }` to ensure we can hoist implicit derivative
    // texture sampling out of larger else blocks where the `texld` op is not first.
    let mut out = vec![0xFFFF0200];
    // if v0
    out.extend(enc_inst(0x0028, &[enc_src(1, 0, 0xE4)]));
    // mov r0, c0
    out.extend(enc_inst(0x0001, &[enc_dst(0, 0, 0xF), enc_src(2, 0, 0xE4)]));
    // else
    out.extend(enc_inst(0x002A, &[]));
    // mov r0, c1
    out.extend(enc_inst(0x0001, &[enc_dst(0, 0, 0xF), enc_src(2, 1, 0xE4)]));
    // texld r1, t0, s0
    out.extend(enc_inst(
        0x0042,
        &[
            enc_dst(0, 1, 0xF),   // r1
            enc_src(3, 0, 0xE4),  // t0
            enc_src(10, 0, 0xE4), // s0
        ],
    ));
    // endif
    out.extend(enc_inst(0x002B, &[]));
    // mov oC0, r1
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 1, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps2_nonuniform_if_texld_then_mov() -> Vec<u32> {
    // ps_2_0
    //
    // Non-uniform `if v0 { texld; mov }` to ensure the legacy translator can hoist implicit-deriv
    // sampling out of multi-statement `if` blocks.
    let mut out = vec![0xFFFF0200];
    // if v0
    out.extend(enc_inst(0x0028, &[enc_src(1, 0, 0xE4)]));
    // texld r0, t0, s0
    out.extend(enc_inst(
        0x0042,
        &[
            enc_dst(0, 0, 0xF),   // r0
            enc_src(3, 0, 0xE4),  // t0
            enc_src(10, 0, 0xE4), // s0
        ],
    ));
    // mov r1, r0
    out.extend(enc_inst(0x0001, &[enc_dst(0, 1, 0xF), enc_src(0, 0, 0xE4)]));
    // endif
    out.extend(enc_inst(0x002B, &[]));
    // mov oC0, r1
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 1, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps2_dp2_masked_xy() -> Vec<u32> {
    // ps_2_0
    let mut out = vec![0xFFFF0200];
    // def c0, 0.5, 0.25, 0.0, 0.0
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            0x3F00_0000,
            0x3E80_0000,
            0x0000_0000,
            0x0000_0000,
        ],
    ));
    // def c1, 0.1, 0.2, 0.3, 0.4
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 1, 0xF),
            0x3DCC_CCCD,
            0x3E4C_CCCD,
            0x3E99_999A,
            0x3ECC_CCCD,
        ],
    ));
    // mov r0, c1
    out.extend(enc_inst(0x0001, &[enc_dst(0, 0, 0xF), enc_src(2, 1, 0xE4)]));
    // dp2 r0.xy, c0, c0
    out.extend(enc_inst(
        0x005A,
        &[enc_dst(0, 0, 0x3), enc_src(2, 0, 0xE4), enc_src(2, 0, 0xE4)],
    ));
    // mov oC0, r0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps_mov_sat_neg_c0() -> Vec<u32> {
    // ps_2_0
    let mut out = vec![0xFFFF0200];
    // mov_sat oC0, -c0
    out.extend(enc_inst_with_extra(
        0x0001,
        1u32 << 20, // saturate
        &[
            enc_dst(8, 0, 0xF),
            enc_src_mod(2, 0, 0xE4, 1), // -c0
        ],
    ));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps2_src_modifiers_bias_x2neg_dz() -> Vec<u32> {
    // ps_2_0
    let mut out = vec![0xFFFF0200];
    // mov r0, c0_bias
    out.extend(enc_inst(
        0x0001,
        &[enc_dst(0, 0, 0xF), enc_src_mod(2, 0, 0xE4, 2)],
    ));
    // add r0, r0, c1_x2neg
    out.extend(enc_inst(
        0x0002,
        &[
            enc_dst(0, 0, 0xF),
            enc_src(0, 0, 0xE4),
            enc_src_mod(2, 1, 0xE4, 8),
        ],
    ));
    // mul r0, r0, c2_dz
    out.extend(enc_inst(
        0x0005,
        &[
            enc_dst(0, 0, 0xF),
            enc_src(0, 0, 0xE4),
            enc_src_mod(2, 2, 0xE4, 9),
        ],
    ));
    // mov oC0, r0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps_mrt_solid_color() -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];
    // mov oC0, c0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(2, 0, 0xE4)]));
    // mov oC1, c0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 1, 0xF), enc_src(2, 0, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_vs_passthrough_sm3() -> Vec<u32> {
    let mut out = assemble_vs_passthrough();
    out[0] = 0xFFFE0300; // vs_3_0
    out
}

fn assemble_ps3_tex_ifc_def() -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];
    // def c0, 0.5, 0.0, 1.0, 1.0
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            0x3F00_0000,
            0x0000_0000,
            0x3F80_0000,
            0x3F80_0000,
        ],
    ));
    // texld r0, t0, s0
    out.extend(enc_inst(
        0x0042,
        &[
            enc_dst(0, 0, 0xF),   // r0
            enc_src(3, 0, 0xE4),  // t0
            enc_src(10, 0, 0xE4), // s0
        ],
    ));
    // ifc_lt c0.x, r0.x (compare op 3 = lt)
    out.extend(enc_inst_with_extra(
        0x0029,
        3u32 << 16,
        &[enc_src(2, 0, 0x00), enc_src(0, 0, 0x00)],
    ));
    // mov oC0, r0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    // else
    out.extend(enc_inst(0x002A, &[]));
    // mov oC0, c0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(2, 0, 0xE4)]));
    // endif
    out.extend(enc_inst(0x002B, &[]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_texld_cube() -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];
    // dcl_texcoord0 v0
    out.extend(enc_inst_with_extra_sm3(
        0x001F,
        5u32 << 16,
        &[enc_dst(1, 0, 0xF)],
    ));
    // dcl_cube s0
    out.extend(enc_inst_with_extra_sm3(
        0x001F,
        3u32 << 16,
        &[enc_dst(10, 0, 0xF)],
    ));
    // texld r0, v0, s0
    out.extend(enc_inst_sm3(
        0x0042,
        &[
            enc_dst(0, 0, 0xF),   // r0
            enc_src(1, 0, 0xE4),  // v0
            enc_src(10, 0, 0xE4), // s0
        ],
    ));
    // mov oC0, r0
    out.extend(enc_inst_sm3(
        0x0001,
        &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)],
    ));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_defb_if(branch: bool) -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];
    // def c0, 1.0, 0.0, 0.0, 1.0 (red)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            0x3F80_0000,
            0x0000_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));
    // def c1, 0.0, 1.0, 0.0, 1.0 (green)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 1, 0xF),
            0x0000_0000,
            0x3F80_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));
    // defb b0, <branch>
    out.extend(enc_inst(
        0x0053,
        &[enc_dst(14, 0, 0x0), if branch { 1 } else { 0 }],
    ));
    // if b0
    out.extend(enc_inst(0x0028, &[enc_src(14, 0, 0x00)]));
    // mov oC0, c0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(2, 0, 0xE4)]));
    // else
    out.extend(enc_inst(0x002A, &[]));
    // mov oC0, c1
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(2, 1, 0xE4)]));
    // endif
    out.extend(enc_inst(0x002B, &[]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_defb_if_high_index(branch: bool) -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];
    // def c0, 1.0, 0.0, 0.0, 1.0 (red)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            0x3F80_0000,
            0x0000_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));
    // def c1, 0.0, 1.0, 0.0, 1.0 (green)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 1, 0xF),
            0x0000_0000,
            0x3F80_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));
    // defb b20, <branch>
    out.extend(enc_inst(
        0x0053,
        &[enc_dst(14, 20, 0x0), if branch { 1 } else { 0 }],
    ));
    // if b20
    out.extend(enc_inst(0x0028, &[enc_src(14, 20, 0x00)]));
    // mov oC0, c0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(2, 0, 0xE4)]));
    // else
    out.extend(enc_inst(0x002A, &[]));
    // mov oC0, c1
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(2, 1, 0xE4)]));
    // endif
    out.extend(enc_inst(0x002B, &[]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_predicated_lrp() -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];

    // def c0, 0.25, 0.25, 0.25, 0.25
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            0x3E80_0000,
            0x3E80_0000,
            0x3E80_0000,
            0x3E80_0000,
        ],
    ));
    // def c1, 1.0, 0.0, 0.0, 1.0
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 1, 0xF),
            0x3F80_0000,
            0x0000_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));
    // def c2, 0.0, 1.0, 0.0, 1.0
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 2, 0xF),
            0x0000_0000,
            0x3F80_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));

    // setp_eq p0, c0, c0  (compare op 1 = eq)
    out.extend(enc_inst_with_extra(
        0x005E,
        1u32 << 16,
        &[
            enc_dst(19, 0, 0xF), // p0
            enc_src(2, 0, 0xE4), // c0
            enc_src(2, 0, 0xE4), // c0
        ],
    ));

    // predicated lrp_sat_x2 r0, c0, c1, c2, p0.x
    // - opcode 0x12 (lrp)
    // - predicated flag = bit 28 (0x1000_0000)
    // - result modifier: saturate + x2 shift => mod_bits = 0b0011 => 3<<20
    out.extend(enc_inst_with_extra(
        0x0012,
        0x1000_0000 | (3u32 << 20),
        &[
            enc_dst(0, 0, 0xF),   // r0
            enc_src(2, 0, 0xE4),  // c0 (t)
            enc_src(2, 1, 0xE4),  // c1 (a)
            enc_src(2, 2, 0xE4),  // c2 (b)
            enc_src(19, 0, 0x00), // p0.x predicate token
        ],
    ));

    // mov oC0, r0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_predicated_lrp_operand_count_len() -> Vec<u32> {
    // Same instruction stream as `assemble_ps3_predicated_lrp`, but encoded using the
    // operand-count length field (excluding the opcode token).
    let mut out = vec![0xFFFF0300];

    // def c0, 0.25, 0.25, 0.25, 0.25
    out.extend(enc_inst_operand_count_len(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            0x3E80_0000,
            0x3E80_0000,
            0x3E80_0000,
            0x3E80_0000,
        ],
    ));
    // def c1, 1.0, 0.0, 0.0, 1.0
    out.extend(enc_inst_operand_count_len(
        0x0051,
        &[
            enc_dst(2, 1, 0xF),
            0x3F80_0000,
            0x0000_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));
    // def c2, 0.0, 1.0, 0.0, 1.0
    out.extend(enc_inst_operand_count_len(
        0x0051,
        &[
            enc_dst(2, 2, 0xF),
            0x0000_0000,
            0x3F80_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));

    // setp_eq p0, c0, c0  (compare op 1 = eq)
    out.extend(enc_inst_with_extra_operand_count_len(
        0x005E,
        1u32 << 16,
        &[
            enc_dst(19, 0, 0xF), // p0
            enc_src(2, 0, 0xE4), // c0
            enc_src(2, 0, 0xE4), // c0
        ],
    ));

    // predicated lrp_sat_x2 r0, c0, c1, c2, p0.x
    out.extend(enc_inst_with_extra_operand_count_len(
        0x0012,
        0x1000_0000 | (3u32 << 20),
        &[
            enc_dst(0, 0, 0xF),   // r0
            enc_src(2, 0, 0xE4),  // c0 (t)
            enc_src(2, 1, 0xE4),  // c1 (a)
            enc_src(2, 2, 0xE4),  // c2 (b)
            enc_src(19, 0, 0x00), // p0.x predicate token
        ],
    ));

    // mov oC0, r0
    out.extend(enc_inst_operand_count_len(
        0x0001,
        &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)],
    ));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_rep_operand_count_len() -> Vec<u32> {
    // ps_3_0:
    //   rep i0
    //   endrep
    //   end
    //
    // Encoded using operand-count length fields (excluding the opcode token).
    let mut out = vec![0xFFFF0300];
    // rep i0
    out.extend(enc_inst_operand_count_len(0x0026, &[enc_src(7, 0, 0xE4)]));
    // endrep
    out.extend(enc_inst_operand_count_len(0x0027, &[]));
    // end
    out.push(0x0000FFFF);
    out
}

fn assemble_ps2_unknown_opcode_operand_count_len() -> Vec<u32> {
    // ps_2_0:
    //   def c0, 1,0,0,1
    //   <unknown opcode>
    //   mov oC0, c0
    //   end
    //
    // Encoded using operand-count length fields (excluding the opcode token). This shader is
    // intended to force SM3 translation to fall back to the legacy translator (because of the
    // unknown opcode) while still requiring operand-count length normalization for `shader::parse`
    // to succeed (because of the `def`).
    let mut out = vec![0xFFFF0200];
    // def c0, 1.0, 0.0, 0.0, 1.0
    out.extend(enc_inst_operand_count_len(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            0x3F80_0000,
            0x0000_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));
    // Unknown opcode 0x1234 (no operands).
    out.extend(enc_inst_operand_count_len(0x1234, &[]));
    // mov oC0, c0
    out.extend(enc_inst_operand_count_len(
        0x0001,
        &[enc_dst(8, 0, 0xF), enc_src(2, 0, 0xE4)],
    ));
    out.push(0x0000FFFF);
    out
}

fn assemble_vs2_passthrough_operand_count_len() -> Vec<u32> {
    // vs_2_0: mov oPos, v0; end (operand-count length encoding).
    let mut out = vec![0xFFFE0200];
    out.extend(enc_inst_operand_count_len(
        0x0001,
        &[enc_dst(4, 0, 0xF), enc_src(1, 0, 0xE4)],
    ));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_lrp() -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];

    // def c0, 0.25, 0.25, 0.25, 0.25
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            0x3E80_0000,
            0x3E80_0000,
            0x3E80_0000,
            0x3E80_0000,
        ],
    ));
    // def c1, 1.0, 0.0, 0.0, 1.0
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 1, 0xF),
            0x3F80_0000,
            0x0000_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));
    // def c2, 0.0, 1.0, 0.0, 1.0
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 2, 0xF),
            0x0000_0000,
            0x3F80_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));

    // lrp r0, c0, c1, c2
    out.extend(enc_inst(
        0x0012,
        &[
            enc_dst(0, 0, 0xF),  // r0
            enc_src(2, 0, 0xE4), // c0 (t)
            enc_src(2, 1, 0xE4), // c1 (a)
            enc_src(2, 2, 0xE4), // c2 (b)
        ],
    ));

    // mov oC0, r0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_predicated_mov() -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];
    // def c0, 0.5, 0.0, 0.0, 0.0 (threshold)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            0x3F00_0000,
            0x0000_0000,
            0x0000_0000,
            0x0000_0000,
        ],
    ));
    // def c1, 1.0, 0.0, 0.0, 1.0 (red)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 1, 0xF),
            0x3F80_0000,
            0x0000_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));
    // def c2, 0.0, 0.0, 1.0, 1.0 (blue)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 2, 0xF),
            0x0000_0000,
            0x0000_0000,
            0x3F80_0000,
            0x3F80_0000,
        ],
    ));

    // setp_gt p0.x, v0.x, c0.x (compare op 0 = gt)
    out.extend(enc_inst(
        0x005E,
        &[
            enc_dst(19, 0, 0x1), // p0.x
            enc_src(1, 0, 0x00), // v0.x
            enc_src(2, 0, 0x00), // c0.x
        ],
    ));

    // mov oC0, c2 (default blue)
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(2, 2, 0xE4)]));

    // (p0.x) mov oC0, c1 (predicated)
    out.extend(enc_inst_with_extra(
        0x0001,
        0x1000_0000, // predicated flag
        &[
            enc_dst(8, 0, 0xF),
            enc_src(2, 1, 0xE4),
            enc_src(19, 0, 0x00), // p0.x predicate token
        ],
    ));

    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_mova_relative_const() -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];
    // def c1, 1.0, 0.0, 0.0, 1.0 (red)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 1, 0xF),
            0x3F80_0000,
            0x0000_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));
    // def c2, 0.0, 0.0, 1.0, 1.0 (blue)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 2, 0xF),
            0x0000_0000,
            0x0000_0000,
            0x3F80_0000,
            0x3F80_0000,
        ],
    ));

    // mova_x2 a0.x, t0.x
    // This exercises:
    // - pixel-shader `mova` destination decoding (regtype 3 -> address register)
    // - result modifier ordering (shift applied before float->int conversion)
    out.extend(enc_inst_with_extra(
        0x002E,
        2u32 << 20, // result shift = x2 (no saturate)
        &[
            enc_dst(3, 0, 0x1),  // a0.x (regtype 3)
            enc_src(3, 0, 0x00), // t0.x
        ],
    ));

    // mov oC0, c1[a0.x]
    let mut c1_rel = enc_src(2, 1, 0xE4);
    c1_rel |= 0x0000_2000; // RELATIVE flag
    out.extend(enc_inst(
        0x0001,
        &[
            enc_dst(8, 0, 0xF),
            c1_rel,
            enc_src(3, 0, 0x00), // a0.x (swizzle xxxx)
        ],
    ));

    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_mova_sat_relative_const() -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];
    // def c0, 2.0, 0.0, 0.0, 0.0 (mova source)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            0x4000_0000, // 2.0
            0x0000_0000,
            0x0000_0000,
            0x0000_0000,
        ],
    ));
    // def c2, 1.0, 0.0, 0.0, 1.0 (red)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 2, 0xF),
            0x3F80_0000,
            0x0000_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));
    // def c3, 0.0, 1.0, 0.0, 1.0 (green)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 3, 0xF),
            0x0000_0000,
            0x3F80_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));

    // mova_sat a0.x, c0.x
    // With saturate, 2.0 clamps to 1.0 and then truncates to int 1.
    out.extend(enc_inst_with_extra(
        0x002E,
        1u32 << 20, // saturate (no shift)
        &[
            enc_dst(3, 0, 0x1),  // a0.x (regtype 3)
            enc_src(2, 0, 0x00), // c0.x
        ],
    ));

    // mov oC0, c1[a0.x]
    // With a0.x = 1, this reads `c2` (red). If saturate were ignored, we'd read `c3` (green).
    let mut c1_rel = enc_src(2, 1, 0xE4);
    c1_rel |= 0x0000_2000; // RELATIVE flag
    out.extend(enc_inst(
        0x0001,
        &[
            enc_dst(8, 0, 0xF),
            c1_rel,
            enc_src(3, 0, 0x00), // a0.x
        ],
    ));

    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_mova_sat_div2_relative_const() -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];
    // def c0, 2.0, 0.0, 0.0, 0.0 (mova source)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            0x4000_0000, // 2.0
            0x0000_0000,
            0x0000_0000,
            0x0000_0000,
        ],
    ));
    // def c1, 0.0, 1.0, 0.0, 1.0 (green)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 1, 0xF),
            0x0000_0000,
            0x3F80_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));
    // def c2, 1.0, 0.0, 1.0, 1.0 (magenta)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 2, 0xF),
            0x3F80_0000,
            0x0000_0000,
            0x3F80_0000,
            0x3F80_0000,
        ],
    ));

    // mova_sat_div2 a0.x, c0.x
    // The SM3 result modifier order is shift then saturate. With source 2.0:
    //   div2 => 1.0, saturate => 1.0, trunc => 1.
    // If saturate was applied before div2, we'd get:
    //   saturate => 1.0, div2 => 0.5, trunc => 0.
    out.extend(enc_inst_with_extra(
        0x002E,
        (9u32) << 20, // result shift = div2 (4) + saturate bit => mod_bits = 1 | (4<<1) = 9
        &[
            enc_dst(3, 0, 0x1),  // a0.x (regtype 3)
            enc_src(2, 0, 0x00), // c0.x
        ],
    ));

    // mov oC0, c1[a0.x]
    // With a0.x = 1, this reads `c2` (magenta). With a0.x = 0, we'd read `c1` (green).
    let mut c1_rel = enc_src(2, 1, 0xE4);
    c1_rel |= 0x0000_2000; // RELATIVE flag
    out.extend(enc_inst(
        0x0001,
        &[
            enc_dst(8, 0, 0xF),
            c1_rel,
            enc_src(3, 0, 0x00), // a0.x
        ],
    ));

    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_predicated_mova_relative_const() -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];
    // def c0, 0.0, 0.0, 0.0, 0.0
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            0x0000_0000,
            0x0000_0000,
            0x0000_0000,
            0x0000_0000,
        ],
    ));
    // def c1, 1.0, 0.0, 0.0, 1.0 (red) (expected output)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 1, 0xF),
            0x3F80_0000,
            0x0000_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));
    // def c2, 0.0, 1.0, 0.0, 1.0 (green) (output if predication is ignored)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 2, 0xF),
            0x0000_0000,
            0x3F80_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));
    // def c3, 1.0, 0.0, 0.0, 0.0 (mova source)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 3, 0xF),
            0x3F80_0000,
            0x0000_0000,
            0x0000_0000,
            0x0000_0000,
        ],
    ));

    // setp_gt p0.x, c0.x, c0.x  (compare op 0 = gt; 0.0 > 0.0 is false)
    out.extend(enc_inst(
        0x004E,
        &[
            enc_dst(19, 0, 0x1), // p0.x
            enc_src(2, 0, 0x00), // c0.x
            enc_src(2, 0, 0x00), // c0.x
        ],
    ));

    // (p0.x) mova a0.x, c3.x  (predicated; should not execute)
    out.extend(enc_inst_with_extra(
        0x002E,
        0x1000_0000, // predicated flag
        &[
            enc_dst(3, 0, 0x1),   // a0.x (regtype 3)
            enc_src(2, 3, 0x00),  // c3.x
            enc_src(19, 0, 0x00), // p0.x predicate token
        ],
    ));

    // mov oC0, c1[a0.x]
    let mut c1_rel = enc_src(2, 1, 0xE4);
    c1_rel |= 0x0000_2000; // RELATIVE flag
    out.extend(enc_inst(
        0x0001,
        &[
            enc_dst(8, 0, 0xF),
            c1_rel,
            enc_src(3, 0, 0x00), // a0.x
        ],
    ));

    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_predicated_mova_relative_const_taken() -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];
    // def c0, 0.0, 0.0, 0.0, 0.0
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            0x0000_0000,
            0x0000_0000,
            0x0000_0000,
            0x0000_0000,
        ],
    ));
    // def c1, 1.0, 0.0, 0.0, 1.0 (red)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 1, 0xF),
            0x3F80_0000,
            0x0000_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));
    // def c2, 0.0, 1.0, 0.0, 1.0 (green) (expected output)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 2, 0xF),
            0x0000_0000,
            0x3F80_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));
    // def c3, 1.0, 0.0, 0.0, 0.0 (mova source)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 3, 0xF),
            0x3F80_0000,
            0x0000_0000,
            0x0000_0000,
            0x0000_0000,
        ],
    ));

    // setp_gt p0.x, c3.x, c0.x  (1.0 > 0.0 is true)
    out.extend(enc_inst(
        0x004E,
        &[
            enc_dst(19, 0, 0x1), // p0.x
            enc_src(2, 3, 0x00), // c3.x
            enc_src(2, 0, 0x00), // c0.x
        ],
    ));

    // (p0.x) mova a0.x, c3.x  (predicated; should execute)
    out.extend(enc_inst_with_extra(
        0x002E,
        0x1000_0000, // predicated flag
        &[
            enc_dst(3, 0, 0x1),   // a0.x (regtype 3)
            enc_src(2, 3, 0x00),  // c3.x
            enc_src(19, 0, 0x00), // p0.x predicate token
        ],
    ));

    // mov oC0, c1[a0.x]
    // With a0.x = 1, this reads `c2` (green).
    let mut c1_rel = enc_src(2, 1, 0xE4);
    c1_rel |= 0x0000_2000; // RELATIVE flag
    out.extend(enc_inst(
        0x0001,
        &[
            enc_dst(8, 0, 0xF),
            c1_rel,
            enc_src(3, 0, 0x00), // a0.x
        ],
    ));

    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_mova_write_mask_preserves_other_components() -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];
    // def c0, 1.0, 1.0, 0.0, 0.0
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            0x3F80_0000,
            0x3F80_0000,
            0x0000_0000,
            0x0000_0000,
        ],
    ));
    // def c1, 1.0, 0.0, 0.0, 1.0 (red)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 1, 0xF),
            0x3F80_0000,
            0x0000_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));
    // def c2, 0.0, 0.0, 1.0, 1.0 (blue)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 2, 0xF),
            0x0000_0000,
            0x0000_0000,
            0x3F80_0000,
            0x3F80_0000,
        ],
    ));

    // mova a0.x, c0.z (0.0)
    out.extend(enc_inst(
        0x002E,
        &[
            enc_dst(3, 0, 0x1),  // a0.x
            enc_src(2, 0, 0xAA), // c0.zzzz
        ],
    ));
    // mova a0.y, c0 (1.0)
    // This must only update `a0.y`; if it overwrites other components (e.g. `a0.x`) then the
    // relative-constant reads below will select the wrong registers.
    out.extend(enc_inst(
        0x002E,
        &[
            enc_dst(3, 0, 0x2),  // a0.y
            enc_src(2, 0, 0xE4), // c0.xyzw
        ],
    ));

    // mov r0, c1[a0.x] (expects c1 since a0.x should remain 0)
    let mut c1_rel_x = enc_src(2, 1, 0xE4);
    c1_rel_x |= 0x0000_2000; // RELATIVE flag
    out.extend(enc_inst(
        0x0001,
        &[
            enc_dst(0, 0, 0xF),
            c1_rel_x,
            enc_src(3, 0, 0x00), // a0.x
        ],
    ));

    // add r0, r0, c1[a0.y] (adds c2 since a0.y == 1)
    let mut c1_rel_y = enc_src(2, 1, 0xE4);
    c1_rel_y |= 0x0000_2000; // RELATIVE flag
    out.extend(enc_inst(
        0x0002,
        &[
            enc_dst(0, 0, 0xF),
            enc_src(0, 0, 0xE4),
            c1_rel_y,
            enc_src(3, 0, 0x55), // a0.y
        ],
    ));

    // mov oC0, r0
    out.extend(enc_inst(
        0x0001,
        &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)],
    ));

    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_mova_multi_component_relative_const() -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];
    // def c0, 1.0, 2.0, 0.0, 0.0 (write a0.x=1, a0.y=2)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            0x3F80_0000, // 1.0
            0x4000_0000, // 2.0
            0x0000_0000,
            0x0000_0000,
        ],
    ));
    // def c2, 1.0, 0.0, 0.0, 1.0 (red)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 2, 0xF),
            0x3F80_0000,
            0x0000_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));
    // def c3, 0.0, 1.0, 0.0, 1.0 (green)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 3, 0xF),
            0x0000_0000,
            0x3F80_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));

    // mova a0.xy, c0.xy
    out.extend(enc_inst(
        0x002E,
        &[
            enc_dst(3, 0, 0x3),  // a0.xy (regtype 3)
            enc_src(2, 0, 0xE4), // c0.xyzw
        ],
    ));

    // mov r0, c1[a0.x]
    let mut c1_rel_x = enc_src(2, 1, 0xE4);
    c1_rel_x |= 0x0000_2000; // RELATIVE flag
    out.extend(enc_inst(
        0x0001,
        &[
            enc_dst(0, 0, 0xF),
            c1_rel_x,
            enc_src(3, 0, 0x00), // a0.x
        ],
    ));

    // add r0, r0, c1[a0.y]
    let mut c1_rel_y = enc_src(2, 1, 0xE4);
    c1_rel_y |= 0x0000_2000; // RELATIVE flag
    out.extend(enc_inst(
        0x0002,
        &[
            enc_dst(0, 0, 0xF),
            enc_src(0, 0, 0xE4),
            c1_rel_y,
            enc_src(3, 0, 0x55), // a0.y (swizzle yyyy)
        ],
    ));

    // mov oC0, r0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));

    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_mova_relative_const_component_y() -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];
    // def c0, 0.0, 1.0, 0.0, 0.0 (index = 1 in the Y component)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            0x0000_0000,
            0x3F80_0000,
            0x0000_0000,
            0x0000_0000,
        ],
    ));
    // def c1, 1.0, 0.0, 0.0, 1.0 (red)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 1, 0xF),
            0x3F80_0000,
            0x0000_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));
    // def c2, 0.0, 0.0, 1.0, 1.0 (blue)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 2, 0xF),
            0x0000_0000,
            0x0000_0000,
            0x3F80_0000,
            0x3F80_0000,
        ],
    ));

    // mova a0.y, c0.y
    out.extend(enc_inst(
        0x002E,
        &[
            enc_dst(3, 0, 0x2),  // a0.y
            enc_src(2, 0, 0xE4), // c0.xyzw
        ],
    ));

    // mov oC0, c1[a0.y]
    // If we incorrectly read a0.x instead, we'd produce c1 (red) rather than c2 (blue).
    let mut c1_rel = enc_src(2, 1, 0xE4);
    c1_rel |= 0x0000_2000; // RELATIVE flag
    out.extend(enc_inst(
        0x0001,
        &[
            enc_dst(8, 0, 0xF),
            c1_rel,
            enc_src(3, 0, 0x55), // a0.y (swizzle yyyy)
        ],
    ));

    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_relative_const_clamp_low() -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];
    // def c0, 1.0, 0.0, 0.0, 1.0 (red)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            0x3F80_0000,
            0x0000_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));
    // def c2, -1000.0, 0.0, 0.0, 0.0 (large negative offset)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 2, 0xF),
            0xC47A_0000, // -1000.0
            0x0000_0000,
            0x0000_0000,
            0x0000_0000,
        ],
    ));

    // mova a0.x, c2.x
    out.extend(enc_inst(0x002E, &[enc_dst(3, 0, 0x1), enc_src(2, 2, 0x00)]));

    // mov oC0, c1[a0.x]
    // With a0.x = -1000, the index clamps to 0, so we read `c0` (red).
    let mut c1_rel = enc_src(2, 1, 0xE4);
    c1_rel |= 0x0000_2000; // RELATIVE flag
    out.extend(enc_inst(
        0x0001,
        &[
            enc_dst(8, 0, 0xF),
            c1_rel,
            enc_src(3, 0, 0x00), // a0.x
        ],
    ));

    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_relative_const_clamp_high() -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];
    // def c0, 1000.0, 0.0, 0.0, 0.0 (large positive offset)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            0x447A_0000, // 1000.0
            0x0000_0000,
            0x0000_0000,
            0x0000_0000,
        ],
    ));
    // def c255, 0.0, 1.0, 0.0, 1.0 (green)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 255, 0xF),
            0x0000_0000,
            0x3F80_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));

    // mova a0.x, c0.x
    out.extend(enc_inst(0x002E, &[enc_dst(3, 0, 0x1), enc_src(2, 0, 0x00)]));

    // mov oC0, c254[a0.x]
    // With a0.x = 1000, the index clamps to 255, so we read `c255` (green).
    let mut c254_rel = enc_src(2, 254, 0xE4);
    c254_rel |= 0x0000_2000; // RELATIVE flag
    out.extend(enc_inst(
        0x0001,
        &[
            enc_dst(8, 0, 0xF),
            c254_rel,
            enc_src(3, 0, 0x00), // a0.x
        ],
    ));

    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_loop_relative_const() -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];
    // def c0, 1.0, 0.0, 0.0, 1.0 (red)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            0x3F80_0000,
            0x0000_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));
    // def c1, 0.0, 0.0, 1.0, 1.0 (blue)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 1, 0xF),
            0x0000_0000,
            0x0000_0000,
            0x3F80_0000,
            0x3F80_0000,
        ],
    ));
    // def c2, 0.0, 0.0, 0.0, 0.0 (zero)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 2, 0xF),
            0x0000_0000,
            0x0000_0000,
            0x0000_0000,
            0x0000_0000,
        ],
    ));

    // defi i0, start=0, end=1, step=1, unused=0
    out.extend(enc_inst(0x0052, &[enc_dst(7, 0, 0xF), 0, 1, 1, 0]));

    // mov r0, c2
    out.extend(enc_inst(0x0001, &[enc_dst(0, 0, 0xF), enc_src(2, 2, 0xE4)]));

    // loop aL, i0
    out.extend(enc_inst(
        0x001B,
        &[
            enc_src(15, 0, 0xE4), // aL
            enc_src(7, 0, 0xE4),  // i0
        ],
    ));

    // add r0, r0, c0[aL.x]  (adds c0 then c1 over two loop iterations)
    let mut c0_rel = enc_src(2, 0, 0xE4);
    c0_rel |= 0x0000_2000; // RELATIVE flag
    out.extend(enc_inst(
        0x0002,
        &[
            enc_dst(0, 0, 0xF),
            enc_src(0, 0, 0xE4),
            c0_rel,
            enc_src(15, 0, 0x00), // aL.x
        ],
    ));

    // endloop
    out.extend(enc_inst(0x001D, &[]));

    // mov oC0, r0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));

    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_rep_relative_const() -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];
    // def c0, 1.0, 0.0, 0.0, 1.0 (red)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            0x3F80_0000,
            0x0000_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));
    // def c1, 0.0, 0.0, 1.0, 1.0 (blue)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 1, 0xF),
            0x0000_0000,
            0x0000_0000,
            0x3F80_0000,
            0x3F80_0000,
        ],
    ));
    // def c2, 0.0, 0.0, 0.0, 0.0 (zero)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 2, 0xF),
            0x0000_0000,
            0x0000_0000,
            0x0000_0000,
            0x0000_0000,
        ],
    ));
    // defi i0, count=2, unused=0,0,0
    out.extend(enc_inst(0x0052, &[enc_dst(7, 0, 0xF), 2, 0, 0, 0]));

    // mov r0, c2
    out.extend(enc_inst(0x0001, &[enc_dst(0, 0, 0xF), enc_src(2, 2, 0xE4)]));

    // rep i0
    out.extend(enc_inst(0x0026, &[enc_src(7, 0, 0xE4)]));

    // add r0, r0, c0[aL.x]  (adds c0 then c1 over two iterations)
    let mut c0_rel = enc_src(2, 0, 0xE4);
    c0_rel |= 0x0000_2000; // RELATIVE flag
    out.extend(enc_inst(
        0x0002,
        &[
            enc_dst(0, 0, 0xF),
            enc_src(0, 0, 0xE4),
            c0_rel,
            enc_src(15, 0, 0x00), // aL.x
        ],
    ));

    // endrep
    out.extend(enc_inst(0x0027, &[]));

    // mov oC0, r0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));

    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_rep_relative_const_high_int_index() -> Vec<u32> {
    // Like `assemble_ps3_rep_relative_const`, but uses `i20` for the rep count. This ensures our
    // software interpreter honors `defi` for integer constant register indices beyond 0..15 (the
    // strict D3D9 limit), matching the rest of Aero's SM3 pipeline which tolerates indices up to
    // `MAX_D3D9_SHADER_REGISTER_INDEX`.
    let mut out = vec![0xFFFF0300];
    // def c0, 1.0, 0.0, 0.0, 1.0 (red)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            0x3F80_0000,
            0x0000_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));
    // def c1, 0.0, 0.0, 1.0, 1.0 (blue)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 1, 0xF),
            0x0000_0000,
            0x0000_0000,
            0x3F80_0000,
            0x3F80_0000,
        ],
    ));
    // def c2, 0.0, 0.0, 0.0, 0.0 (zero)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 2, 0xF),
            0x0000_0000,
            0x0000_0000,
            0x0000_0000,
            0x0000_0000,
        ],
    ));
    // defi i20, count=2, unused=0,0,0
    out.extend(enc_inst(0x0052, &[enc_dst(7, 20, 0xF), 2, 0, 0, 0]));

    // mov r0, c2
    out.extend(enc_inst(0x0001, &[enc_dst(0, 0, 0xF), enc_src(2, 2, 0xE4)]));

    // rep i20
    out.extend(enc_inst(0x0026, &[enc_src(7, 20, 0xE4)]));

    // add r0, r0, c0[aL.x]  (adds c0 then c1 over two iterations)
    let mut c0_rel = enc_src(2, 0, 0xE4);
    c0_rel |= 0x0000_2000; // RELATIVE flag
    out.extend(enc_inst(
        0x0002,
        &[
            enc_dst(0, 0, 0xF),
            enc_src(0, 0, 0xE4),
            c0_rel,
            enc_src(15, 0, 0x00), // aL.x
        ],
    ));

    // endrep
    out.extend(enc_inst(0x0027, &[]));

    // mov oC0, r0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));

    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_dp2_constant() -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];
    // def c0, 0.0, 0.0, 0.25, 0.5
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            0x0000_0000,
            0x0000_0000,
            0x3E80_0000,
            0x3F00_0000,
        ],
    ));
    // def c1, 0.5, 1.0, 0.0, 0.0
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 1, 0xF),
            0x3F00_0000,
            0x3F80_0000,
            0x0000_0000,
            0x0000_0000,
        ],
    ));
    // def c2, 1.0, 0.0, 0.0, 0.0 (alpha)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 2, 0xF),
            0x3F80_0000,
            0x0000_0000,
            0x0000_0000,
            0x0000_0000,
        ],
    ));

    // dp2 r0.xyz, c0.zwxy, c1.yxwz
    out.extend(enc_inst(
        0x005A,
        &[
            enc_dst(0, 0, 0x7),
            enc_src(2, 0, 0x4E), // c0.zwxy
            enc_src(2, 1, 0xB1), // c1.yxwz
        ],
    ));
    // mov r0.w, c2.x
    out.extend(enc_inst(0x0001, &[enc_dst(0, 0, 0x8), enc_src(2, 2, 0x00)]));
    // mov oC0, r0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));

    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_loop_accumulate() -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];
    // def c0, 0.0, 0.0, 0.0, 1.0 (base)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            0x0000_0000,
            0x0000_0000,
            0x0000_0000,
            0x3F80_0000,
        ],
    ));
    // def c1, 0.1, 0.2, 0.3, 0.0 (increment)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 1, 0xF),
            0x3DCC_CCCD, // 0.1
            0x3E4C_CCCD, // 0.2
            0x3E99_999A, // 0.3
            0x0000_0000,
        ],
    ));
    // def c2, 1.0, 0.0, 0.0, 0.0 (counter step)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 2, 0xF),
            0x3F80_0000,
            0x0000_0000,
            0x0000_0000,
            0x0000_0000,
        ],
    ));
    // def c3, 4.0, 0.0, 0.0, 0.0 (loop count)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 3, 0xF),
            0x4080_0000,
            0x0000_0000,
            0x0000_0000,
            0x0000_0000,
        ],
    ));

    // defi i0, 0, 3, 1, 0 (loop start/end/step)
    out.extend(enc_inst(
        0x0052,
        &[
            enc_dst(7, 0, 0xF), // i0
            0,
            3,
            1,
            0,
        ],
    ));

    // mov r0, c0
    out.extend(enc_inst(0x0001, &[enc_dst(0, 0, 0xF), enc_src(2, 0, 0xE4)]));
    // mov r1.x, c0.x
    out.extend(enc_inst(0x0001, &[enc_dst(0, 1, 0x1), enc_src(2, 0, 0x00)]));

    // loop aL, i0
    out.extend(enc_inst(
        0x001B,
        &[
            enc_src(15, 0, 0xE4), // aL
            enc_src(7, 0, 0xE4),  // i0
        ],
    ));
    // add r0, r0, c1
    out.extend(enc_inst(
        0x0002,
        &[enc_dst(0, 0, 0xF), enc_src(0, 0, 0xE4), enc_src(2, 1, 0xE4)],
    ));
    // add r1.x, r1.x, c2.x
    out.extend(enc_inst(
        0x0002,
        &[enc_dst(0, 1, 0x1), enc_src(0, 1, 0x00), enc_src(2, 2, 0x00)],
    ));
    // breakc_ge r1.x, c3.x (compare op 2 = ge)
    out.extend(enc_inst_with_extra(
        0x002D,
        2u32 << 16,
        &[enc_src(0, 1, 0x00), enc_src(2, 3, 0x00)],
    ));
    // endloop
    out.extend(enc_inst(0x001D, &[]));

    // mov oC0, r0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));

    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_break_outside_loop() -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];
    // break (invalid: not inside a loop)
    out.extend(enc_inst(0x002C, &[]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_breakc_outside_loop() -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];
    // breakc_ge r0.x, c0.x (invalid: not inside a loop)
    out.extend(enc_inst_with_extra(
        0x002D,
        2u32 << 16, // compare op 2 = ge
        &[enc_src(0, 0, 0x00), enc_src(2, 0, 0x00)],
    ));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_texkill() -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];
    // texkill r0
    out.extend(enc_inst(0x0041, &[enc_src(0, 0, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_vs3_texkill() -> Vec<u32> {
    // vs_3_0
    let mut out = vec![0xFFFE0300];
    // texkill r0 (invalid: texkill/discard is only valid in pixel shaders)
    out.extend(enc_inst(0x0041, &[enc_src(0, 0, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_vs3_reads_vpos_misc_sm3_decoder() -> Vec<u32> {
    // vs_3_0: attempts to read vPos via MiscType (misc0), which is only valid in pixel shaders.
    let mut out = vec![0xFFFE0300];
    // mov r0, misc0
    out.extend(enc_inst(
        0x0001,
        &[enc_dst(0, 0, 0xF), enc_src(17, 0, 0xE4)],
    ));
    // mov oPos, r0
    out.extend(enc_inst(0x0001, &[enc_dst(4, 0, 0xF), enc_src(0, 0, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_reads_misc2_sm3_decoder() -> Vec<u32> {
    // ps_3_0: attempts to read an unsupported MiscType builtin (misc2).
    let mut out = vec![0xFFFF0300];
    // mov r0, misc2
    out.extend(enc_inst(
        0x0001,
        &[enc_dst(0, 0, 0xF), enc_src(17, 2, 0xE4)],
    ));
    // mov oC0, r0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_nrm_sm3_decoder() -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];
    // def c0, 3.0, 4.0, 0.0, 0.0
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            0x4040_0000,
            0x4080_0000,
            0x0000_0000,
            0x0000_0000,
        ],
    ));
    // nrm r0, c0
    out.extend(enc_inst(0x0024, &[enc_dst(0, 0, 0xF), enc_src(2, 0, 0xE4)]));
    // mov oC0, r0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_lit_sm3_decoder() -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];
    // def c0, 0.5, 0.5, 0.0, 2.0
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            0x3F00_0000,
            0x3F00_0000,
            0x0000_0000,
            0x4000_0000,
        ],
    ));
    // lit r0, c0
    out.extend(enc_inst(0x0010, &[enc_dst(0, 0, 0xF), enc_src(2, 0, 0xE4)]));
    // mov oC0, r0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_sincos_sm3_decoder() -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];
    // def c0, 1.0, 0.0, 0.0, 0.0 (angle)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            0x3F80_0000,
            0x0000_0000,
            0x0000_0000,
            0x0000_0000,
        ],
    ));
    // def c1, 2.0, 0.0, 0.0, 0.0 (scale)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 1, 0xF),
            0x4000_0000,
            0x0000_0000,
            0x0000_0000,
            0x0000_0000,
        ],
    ));
    // def c2, 0.5, 0.0, 0.0, 0.0 (bias)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 2, 0xF),
            0x3F00_0000,
            0x0000_0000,
            0x0000_0000,
            0x0000_0000,
        ],
    ));
    // sincos_sat r0, c0, c1, c2
    out.extend(enc_inst_with_extra(
        0x0025,
        1u32 << 20, // saturate
        &[
            enc_dst(0, 0, 0xF),
            enc_src(2, 0, 0xE4),
            enc_src(2, 1, 0xE4),
            enc_src(2, 2, 0xE4),
        ],
    ));
    // mov r0.w, c0.x (set alpha to 1.0)
    out.extend(enc_inst(0x0001, &[enc_dst(0, 0, 0x8), enc_src(2, 0, 0x00)]));
    // mov oC0, r0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_exp_log_pow() -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];
    // def c0, -2.0, -2.0, -2.0, -2.0
    out.extend(enc_inst_sm3(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            0xC000_0000,
            0xC000_0000,
            0xC000_0000,
            0xC000_0000,
        ],
    ));
    // def c1, 2.0, 2.0, 2.0, 2.0
    out.extend(enc_inst_sm3(
        0x0051,
        &[
            enc_dst(2, 1, 0xF),
            0x4000_0000,
            0x4000_0000,
            0x4000_0000,
            0x4000_0000,
        ],
    ));
    // def c2, 0.25, 0.25, 0.25, 0.25
    out.extend(enc_inst_sm3(
        0x0051,
        &[
            enc_dst(2, 2, 0xF),
            0x3E80_0000,
            0x3E80_0000,
            0x3E80_0000,
            0x3E80_0000,
        ],
    ));
    // def c3, 2.0, 2.0, 2.0, 2.0
    out.extend(enc_inst_sm3(
        0x0051,
        &[
            enc_dst(2, 3, 0xF),
            0x4000_0000,
            0x4000_0000,
            0x4000_0000,
            0x4000_0000,
        ],
    ));

    // exp r0, c0
    out.extend(enc_inst_sm3(
        0x000E,
        &[enc_dst(0, 0, 0xF), enc_src(2, 0, 0xE4)],
    ));
    // log r1, c1
    out.extend(enc_inst_sm3(
        0x000F,
        &[enc_dst(0, 1, 0xF), enc_src(2, 1, 0xE4)],
    ));
    // pow r2, c2, c3
    out.extend(enc_inst_sm3(
        0x0020,
        &[enc_dst(0, 2, 0xF), enc_src(2, 2, 0xE4), enc_src(2, 3, 0xE4)],
    ));

    // mov r3, r0
    out.extend(enc_inst_sm3(
        0x0001,
        &[enc_dst(0, 3, 0xF), enc_src(0, 0, 0xE4)],
    ));
    // mov r3.y, r1.x
    out.extend(enc_inst_sm3(
        0x0001,
        &[enc_dst(0, 3, 0x2), enc_src(0, 1, 0x00)],
    ));
    // mov r3.z, r2.x
    out.extend(enc_inst_sm3(
        0x0001,
        &[enc_dst(0, 3, 0x4), enc_src(0, 2, 0x00)],
    ));
    // mov r3.w, r1.x
    out.extend(enc_inst_sm3(
        0x0001,
        &[enc_dst(0, 3, 0x8), enc_src(0, 1, 0x00)],
    ));

    // mov oC0, r3
    out.extend(enc_inst_sm3(
        0x0001,
        &[enc_dst(8, 0, 0xF), enc_src(0, 3, 0xE4)],
    ));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_exp_components() -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];
    // def c0, -2.0, -1.0, 0.0, -3.0
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            (-2.0f32).to_bits(),
            (-1.0f32).to_bits(),
            0.0f32.to_bits(),
            (-3.0f32).to_bits(),
        ],
    ));

    // exp r0, c0
    out.extend(enc_inst(0x000E, &[enc_dst(0, 0, 0xF), enc_src(2, 0, 0xE4)]));

    // mov oC0, r0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_log_components_div8() -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];
    // def c0, 1.0, 2.0, 4.0, 8.0
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            1.0f32.to_bits(),
            2.0f32.to_bits(),
            4.0f32.to_bits(),
            8.0f32.to_bits(),
        ],
    ));

    // log_d8 r0, c0 (modbits: div8)
    out.extend(enc_inst_with_extra(
        0x000F,
        12u32 << 20,
        &[enc_dst(0, 0, 0xF), enc_src(2, 0, 0xE4)],
    ));

    // mov oC0, r0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_pow_components() -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];
    // def c0, 0.25, 0.5, 0.75, 1.0
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            0.25f32.to_bits(),
            0.5f32.to_bits(),
            0.75f32.to_bits(),
            1.0f32.to_bits(),
        ],
    ));
    // def c1, 2.0, 2.0, 2.0, 0.0
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 1, 0xF),
            2.0f32.to_bits(),
            2.0f32.to_bits(),
            2.0f32.to_bits(),
            0.0f32.to_bits(),
        ],
    ));

    // pow r0, c0, c1
    out.extend(enc_inst(
        0x0020,
        &[enc_dst(0, 0, 0xF), enc_src(2, 0, 0xE4), enc_src(2, 1, 0xE4)],
    ));

    // mov oC0, r0
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_exp_log_pow_sat_x2_order() -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];
    // def c0, 1.0, 1.0, 1.0, 1.0
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            1.0f32.to_bits(),
            1.0f32.to_bits(),
            1.0f32.to_bits(),
            1.0f32.to_bits(),
        ],
    ));
    // def c1, 4.0, 4.0, 4.0, 4.0
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 1, 0xF),
            4.0f32.to_bits(),
            4.0f32.to_bits(),
            4.0f32.to_bits(),
            4.0f32.to_bits(),
        ],
    ));
    // def c2, 2.0, 2.0, 2.0, 2.0
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 2, 0xF),
            2.0f32.to_bits(),
            2.0f32.to_bits(),
            2.0f32.to_bits(),
            2.0f32.to_bits(),
        ],
    ));
    // def c3, 0.25, 0.25, 0.25, 0.25
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 3, 0xF),
            0.25f32.to_bits(),
            0.25f32.to_bits(),
            0.25f32.to_bits(),
            0.25f32.to_bits(),
        ],
    ));

    // exp_sat_x2 r0, c0
    out.extend(enc_inst_with_extra(
        0x000E,
        3u32 << 20, // saturate + mul2
        &[enc_dst(0, 0, 0xF), enc_src(2, 0, 0xE4)],
    ));
    // mul r0, r0, c3
    out.extend(enc_inst(
        0x0005,
        &[enc_dst(0, 0, 0xF), enc_src(0, 0, 0xE4), enc_src(2, 3, 0xE4)],
    ));

    // log_sat_x2 r1, c1
    out.extend(enc_inst_with_extra(
        0x000F,
        3u32 << 20, // saturate + mul2
        &[enc_dst(0, 1, 0xF), enc_src(2, 1, 0xE4)],
    ));
    // mul r1, r1, c3
    out.extend(enc_inst(
        0x0005,
        &[enc_dst(0, 1, 0xF), enc_src(0, 1, 0xE4), enc_src(2, 3, 0xE4)],
    ));

    // pow_sat_x2 r2, c2, c2
    out.extend(enc_inst_with_extra(
        0x0020,
        3u32 << 20, // saturate + mul2
        &[enc_dst(0, 2, 0xF), enc_src(2, 2, 0xE4), enc_src(2, 2, 0xE4)],
    ));
    // mul r2, r2, c3
    out.extend(enc_inst(
        0x0005,
        &[enc_dst(0, 2, 0xF), enc_src(0, 2, 0xE4), enc_src(2, 3, 0xE4)],
    ));

    // mov r3, r0
    out.extend(enc_inst(0x0001, &[enc_dst(0, 3, 0xF), enc_src(0, 0, 0xE4)]));
    // mov r3.y, r1.x
    out.extend(enc_inst(0x0001, &[enc_dst(0, 3, 0x2), enc_src(0, 1, 0x00)]));
    // mov r3.z, r2.x
    out.extend(enc_inst(0x0001, &[enc_dst(0, 3, 0x4), enc_src(0, 2, 0x00)]));
    // mov r3.w, c0.x (alpha = 1.0)
    out.extend(enc_inst(0x0001, &[enc_dst(0, 3, 0x8), enc_src(2, 0, 0x00)]));

    // mov oC0, r3
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 3, 0xE4)]));

    out.push(0x0000FFFF);
    out
}

fn assemble_ps3_predicated_exp_log_pow_modifiers() -> Vec<u32> {
    // ps_3_0
    let mut out = vec![0xFFFF0300];
    // def c0, 0.5, 0.0, 0.0, 0.0 (predicate threshold)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            0x3F00_0000,
            0x0000_0000,
            0x0000_0000,
            0x0000_0000,
        ],
    ));
    // def c1, -2.0, -2.0, -2.0, -2.0
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 1, 0xF),
            0xC000_0000,
            0xC000_0000,
            0xC000_0000,
            0xC000_0000,
        ],
    ));
    // def c2, 1.25, 1.25, 1.25, 1.25
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 2, 0xF),
            0x3FA0_0000,
            0x3FA0_0000,
            0x3FA0_0000,
            0x3FA0_0000,
        ],
    ));
    // def c3, 0.25, 0.25, 0.25, 0.25
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 3, 0xF),
            0x3E80_0000,
            0x3E80_0000,
            0x3E80_0000,
            0x3E80_0000,
        ],
    ));
    // def c4, 2.0, 2.0, 2.0, 2.0
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 4, 0xF),
            0x4000_0000,
            0x4000_0000,
            0x4000_0000,
            0x4000_0000,
        ],
    ));
    // def c5, 0.0, 0.0, 1.0, 1.0 (default blue output)
    out.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 5, 0xF),
            0x0000_0000,
            0x0000_0000,
            0x3F80_0000,
            0x3F80_0000,
        ],
    ));

    // setp_gt p0.x, v0.x, c0.x (compare op 0 = gt)
    out.extend(enc_inst(
        0x005E,
        &[
            enc_dst(19, 0, 0x1), // p0.x
            enc_src(1, 0, 0x00), // v0.x
            enc_src(2, 0, 0x00), // c0.x
        ],
    ));

    // mov oC0, c5 (default blue)
    out.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(2, 5, 0xE4)]));

    // (p0.x) exp_sat_x2 r0, c1
    out.extend(enc_inst_with_extra(
        0x000E,
        0x1000_0000 | (3u32 << 20), // predicated + saturate + mul2
        &[
            enc_dst(0, 0, 0xF),
            enc_src(2, 1, 0xE4),
            enc_src(19, 0, 0x00), // p0.x
        ],
    ));
    // (p0.x) log_sat_x2 r1, c2
    out.extend(enc_inst_with_extra(
        0x000F,
        0x1000_0000 | (3u32 << 20), // predicated + saturate + mul2
        &[
            enc_dst(0, 1, 0xF),
            enc_src(2, 2, 0xE4),
            enc_src(19, 0, 0x00), // p0.x
        ],
    ));
    // (p0.x) pow_sat_x2 r2, c3, c4
    out.extend(enc_inst_with_extra(
        0x0020,
        0x1000_0000 | (3u32 << 20), // predicated + saturate + mul2
        &[
            enc_dst(0, 2, 0xF),
            enc_src(2, 3, 0xE4),
            enc_src(2, 4, 0xE4),
            enc_src(19, 0, 0x00), // p0.x
        ],
    ));

    // mov r3, r0
    out.extend(enc_inst(0x0001, &[enc_dst(0, 3, 0xF), enc_src(0, 0, 0xE4)]));
    // mov r3.y, r1.x
    out.extend(enc_inst(0x0001, &[enc_dst(0, 3, 0x2), enc_src(0, 1, 0x00)]));
    // mov r3.z, r2.x
    out.extend(enc_inst(0x0001, &[enc_dst(0, 3, 0x4), enc_src(0, 2, 0x00)]));
    // mov r3.w, c5.w
    out.extend(enc_inst(0x0001, &[enc_dst(0, 3, 0x8), enc_src(2, 5, 0xFF)]));

    // (p0.x) mov oC0, r3
    out.extend(enc_inst_with_extra(
        0x0001,
        0x1000_0000, // predicated flag
        &[
            enc_dst(8, 0, 0xF),
            enc_src(0, 3, 0xE4),
            enc_src(19, 0, 0x00), // p0.x
        ],
    ));
    out.push(0x0000FFFF);
    out
}

fn build_sm3_ir(words: &[u32]) -> sm3::ShaderIr {
    let decoded = sm3::decode_u32_tokens(words).unwrap();
    let ir = sm3::build_ir(&decoded).unwrap();
    sm3::verify_ir(&ir).unwrap();
    ir
}

fn to_bytes(words: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(words.len() * 4);
    for w in words {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    bytes
}

#[test]
fn dxbc_container_roundtrip_extracts_shdr() {
    let vs = to_bytes(&assemble_vs_passthrough());
    let container = dxbc_test_utils::build_container(&[(DxbcFourCC(*b"SHDR"), &vs)]);
    let extracted = dxbc::extract_shader_bytecode(&container).unwrap();
    assert_eq!(extracted, vs);
}

#[test]
fn dxbc_container_roundtrip_extracts_shex() {
    let vs = to_bytes(&assemble_vs_passthrough());
    let container = dxbc_test_utils::build_container(&[(DxbcFourCC(*b"SHEX"), &vs)]);
    let extracted = dxbc::extract_shader_bytecode(&container).unwrap();
    assert_eq!(extracted, vs);
}

#[test]
fn dxbc_extraction_returns_raw_token_streams_unchanged() {
    // D3D9 often provides the legacy SM2/SM3 token stream directly (no DXBC container wrapper).
    // In this case we must treat the bytes as already-being shader bytecode.
    let vs = to_bytes(&assemble_vs_passthrough());
    let extracted = dxbc::extract_shader_bytecode(&vs).unwrap();
    assert_eq!(extracted, vs);
}

#[test]
fn dxbc_container_honors_total_size_when_buffer_has_trailing_bytes() {
    // DXBC headers carry a declared `total_size`. The shared `aero-dxbc` parser should treat any
    // trailing bytes in the backing buffer as out-of-container and ensure chunk slices never
    // reference them.
    let vs = to_bytes(&assemble_vs_passthrough());
    let mut container = dxbc_test_utils::build_container(&[(DxbcFourCC(*b"SHDR"), &vs)]);
    container.extend_from_slice(&[0xaa, 0xbb, 0xcc, 0xdd]); // trailing garbage beyond total_size

    let extracted = dxbc::extract_shader_bytecode(&container).unwrap();
    assert_eq!(extracted, vs);
}

#[test]
fn dxbc_container_missing_shdr_is_an_error() {
    // DXBC containers should always provide the shader bytecode in SHDR/SHEX. If the container is
    // missing those chunks, the caller likely passed the wrong blob (or the guest sent corrupted
    // data).
    let dummy_rdef = [0u8; 4];
    let container = dxbc_test_utils::build_container(&[(DxbcFourCC(*b"RDEF"), &dummy_rdef)]);

    let err = dxbc::extract_shader_bytecode(&container).unwrap_err();
    assert!(
        matches!(err, dxbc::DxbcError::MissingShaderChunk),
        "{err:?}"
    );
}

#[test]
fn translates_simple_vs_to_wgsl() {
    let vs_bytes = to_bytes(&assemble_vs_passthrough());
    let dxbc = dxbc_test_utils::build_container(&[(DxbcFourCC(*b"SHDR"), &vs_bytes)]);
    let program = shader::parse(&dxbc).unwrap();
    let ir = shader::to_ir(&program);
    let wgsl = shader::generate_wgsl(&ir).unwrap();

    // Validate WGSL via naga to ensure WebGPU compatibility.
    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    let _info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(wgsl.wgsl.contains("@vertex"));
    assert!(wgsl.wgsl.contains("fn vs_main"));
    assert!(wgsl.wgsl.contains("@builtin(position)"));
}

#[test]
fn translate_entrypoint_prefers_sm3_when_supported() {
    let ps_bytes = to_bytes(&assemble_ps3_predicated_lrp());
    let translated =
        shader_translate::translate_d3d9_shader_to_wgsl(&ps_bytes, shader::WgslOptions::default())
            .unwrap();
    assert_eq!(
        translated.backend,
        shader_translate::ShaderTranslateBackend::Sm3
    );

    let module = naga::front::wgsl::parse_str(&translated.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
    assert!(translated.wgsl.contains("@fragment"));
    assert_eq!(translated.entry_point, "fs_main");
}

#[test]
fn translate_entrypoint_sm3_half_pixel_center_affects_vertex_wgsl() {
    let vs_bytes = to_bytes(&assemble_vs_passthrough_sm3_decoder());

    let translated_enabled = shader_translate::translate_d3d9_shader_to_wgsl(
        &vs_bytes,
        shader::WgslOptions {
            half_pixel_center: true,
        },
    )
    .unwrap();
    assert_eq!(
        translated_enabled.backend,
        shader_translate::ShaderTranslateBackend::Sm3
    );
    assert!(translated_enabled.wgsl.contains("@vertex"));
    assert!(
        translated_enabled
            .wgsl
            .contains("@group(3) @binding(0) var<uniform> half_pixel: HalfPixel;"),
        "{}",
        translated_enabled.wgsl
    );
    assert!(
        translated_enabled
            .wgsl
            .contains("out.pos.x = out.pos.x - half_pixel.inv_viewport.x * out.pos.w;"),
        "{}",
        translated_enabled.wgsl
    );
    assert!(
        translated_enabled
            .wgsl
            .contains("out.pos.y = out.pos.y + half_pixel.inv_viewport.y * out.pos.w;"),
        "{}",
        translated_enabled.wgsl
    );

    // Validate WGSL via naga to ensure WebGPU compatibility.
    let module = naga::front::wgsl::parse_str(&translated_enabled.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    let translated_disabled =
        shader_translate::translate_d3d9_shader_to_wgsl(&vs_bytes, shader::WgslOptions::default())
            .unwrap();
    assert_eq!(
        translated_disabled.backend,
        shader_translate::ShaderTranslateBackend::Sm3
    );
    assert!(
        !translated_disabled
            .wgsl
            .contains("@group(3) @binding(0) var<uniform> half_pixel: HalfPixel;"),
        "{}",
        translated_disabled.wgsl
    );
    assert!(
        !translated_disabled.wgsl.contains("half_pixel.inv_viewport"),
        "{}",
        translated_disabled.wgsl
    );
}

#[test]
fn translate_entrypoint_accepts_operand_count_length_encoding() {
    // Some historical shader blobs encoded opcode token length as operand count instead of total
    // instruction length. The high-level translator should normalize this and still produce valid
    // WGSL.
    let ps_bytes = to_bytes(&assemble_ps3_predicated_lrp_operand_count_len());
    let translated =
        shader_translate::translate_d3d9_shader_to_wgsl(&ps_bytes, shader::WgslOptions::default())
            .unwrap();
    assert_eq!(
        translated.backend,
        shader_translate::ShaderTranslateBackend::Sm3
    );

    let module = naga::front::wgsl::parse_str(&translated.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
    assert!(translated.wgsl.contains("@fragment"));
    assert_eq!(translated.entry_point, "fs_main");
}

#[test]
fn translate_entrypoint_accepts_operand_count_length_encoding_with_rep() {
    // Regression: the operand-count length normalization heuristic should understand `rep`, so
    // operand-count-encoded token streams that mostly consist of control flow still translate.
    let ps_bytes = to_bytes(&assemble_ps3_rep_operand_count_len());
    let translated =
        shader_translate::translate_d3d9_shader_to_wgsl(&ps_bytes, shader::WgslOptions::default())
            .unwrap();
    assert_eq!(
        translated.backend,
        shader_translate::ShaderTranslateBackend::Sm3
    );

    assert!(
        translated.wgsl.contains("_aero_rep_count"),
        "{}",
        translated.wgsl
    );

    let module = naga::front::wgsl::parse_str(&translated.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
    assert!(translated.wgsl.contains("@fragment"));
    assert_eq!(translated.entry_point, "fs_main");
}

#[test]
fn translate_entrypoint_fallback_accepts_operand_count_length_encoding() {
    // The high-level translator should normalize operand-count length encoding before invoking the
    // legacy fallback translator (which expects total-length instruction encoding).
    //
    // This shader forces fallback by including an unknown opcode, but still contains a `def`
    // instruction which will be mis-parsed (truncated) unless length normalization happens.
    let ps_bytes = to_bytes(&assemble_ps2_unknown_opcode_operand_count_len());
    let translated =
        shader_translate::translate_d3d9_shader_to_wgsl(&ps_bytes, shader::WgslOptions::default())
            .unwrap();
    assert_eq!(
        translated.backend,
        shader_translate::ShaderTranslateBackend::LegacyFallback
    );

    let module = naga::front::wgsl::parse_str(&translated.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
    assert!(translated.wgsl.contains("@fragment"));
    assert_eq!(translated.entry_point, "fs_main");
}

#[test]
fn shader_parse_accepts_operand_count_length_encoding() {
    // `aero_d3d9::shader::parse` is used by tooling/tests and should accept the operand-count
    // instruction-length encoding used by some historical shaders.
    let ps_bytes = to_bytes(&assemble_ps2_unknown_opcode_operand_count_len());
    let program = shader::parse(&ps_bytes).unwrap();
    assert_eq!(program.version.stage, shader::ShaderStage::Pixel);
    assert_eq!(program.version.model.major, 2);
    assert_eq!(program.version.model.minor, 0);
    assert!(program.const_defs_f32.contains_key(&0));
}

#[test]
fn translate_entrypoint_accepts_operand_count_length_encoding_in_dxbc_container() {
    // Ensure operand-count-encoded raw token streams remain supported when wrapped in a DXBC
    // container (the runtime commonly receives DXBC blobs from D3DCompile).
    let vs_bytes = to_bytes(&assemble_vs2_passthrough_operand_count_len());
    let dxbc = dxbc_test_utils::build_container(&[(DxbcFourCC(*b"SHDR"), &vs_bytes)]);
    let translated =
        shader_translate::translate_d3d9_shader_to_wgsl(&dxbc, shader::WgslOptions::default())
            .unwrap();
    assert_eq!(
        translated.backend,
        shader_translate::ShaderTranslateBackend::Sm3
    );

    let module = naga::front::wgsl::parse_str(&translated.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
    assert!(translated.wgsl.contains("@vertex"));
    assert_eq!(translated.entry_point, "vs_main");
}

#[test]
fn sm3_translate_to_wgsl_accepts_operand_count_length_encoding() {
    // `aero_d3d9::sm3::wgsl::translate_to_wgsl` is used by `aero-gpu`'s experimental
    // `D3d9ShaderCache`. Ensure it accepts the operand-count length encoding used by some legacy
    // shader blobs.
    let vs_bytes = to_bytes(&assemble_vs2_passthrough_operand_count_len());
    let translated = crate::sm3::wgsl::translate_to_wgsl(&vs_bytes).unwrap();
    assert!(translated.wgsl.contains("@vertex"));
    assert_eq!(translated.entry_point, "vs_main");

    let module = naga::front::wgsl::parse_str(&translated.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn normalize_sm2_sm3_instruction_lengths_patches_rep() {
    let ps_bytes = to_bytes(&assemble_ps3_rep_operand_count_len());
    let normalized = crate::token_stream::normalize_sm2_sm3_instruction_lengths(&ps_bytes).unwrap();

    // rep opcode token is the first instruction token after the version DWORD.
    let bytes = &normalized.as_ref()[4..8];
    let token = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let len_field = (token >> 24) & 0x0F;
    assert_eq!(len_field, 2, "expected rep to be patched to total length 2");
}

#[test]
fn translate_entrypoint_sm3_supports_texcoord8_vertex_inputs() {
    // TEXCOORD8 is outside the fixed StandardLocationMap TEXCOORD0..7 range. Ensure the SM3
    // translator uses the adaptive semantic mapping and does not fall back to the legacy
    // translator.
    let vs_bytes = to_bytes(&assemble_vs_passthrough_with_texcoord8_dcl_sm3_decoder());
    let translated =
        shader_translate::translate_d3d9_shader_to_wgsl(&vs_bytes, shader::WgslOptions::default())
            .unwrap();
    assert_eq!(
        translated.backend,
        shader_translate::ShaderTranslateBackend::Sm3
    );
    assert!(translated.uses_semantic_locations);

    let tex0 = translated
        .semantic_locations
        .iter()
        .find(|s| s.usage == crate::vertex::DeclUsage::TexCoord && s.usage_index == 0)
        .unwrap();
    assert_eq!(tex0.location, 8);

    let tex8 = translated
        .semantic_locations
        .iter()
        .find(|s| s.usage == crate::vertex::DeclUsage::TexCoord && s.usage_index == 8)
        .unwrap();
    assert_eq!(tex8.location, 1);

    // Validate WGSL via naga to ensure WebGPU compatibility.
    let module = naga::front::wgsl::parse_str(&translated.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn translate_entrypoint_sm3_remaps_unused_declared_semantics() {
    // Regression: when the SM3 pipeline performs semantic remapping, `semantic_locations` should
    // report canonical locations for *all* declared input semantics, even if some are unused by
    // the instruction stream. Otherwise host-side vertex input binding can see collisions between
    // remapped and non-remapped declarations.
    let vs_bytes =
        to_bytes(&assemble_vs_passthrough_with_texcoord8_and_unused_normal_dcl_sm3_decoder());
    let translated =
        shader_translate::translate_d3d9_shader_to_wgsl(&vs_bytes, shader::WgslOptions::default())
            .unwrap();
    assert_eq!(
        translated.backend,
        shader_translate::ShaderTranslateBackend::Sm3
    );
    assert!(translated.uses_semantic_locations);

    let normal0 = translated
        .semantic_locations
        .iter()
        .find(|s| s.usage == crate::vertex::DeclUsage::Normal && s.usage_index == 0)
        .unwrap();
    assert_eq!(normal0.location, 1);

    let tex8 = translated
        .semantic_locations
        .iter()
        .find(|s| s.usage == crate::vertex::DeclUsage::TexCoord && s.usage_index == 8)
        .unwrap();
    assert_eq!(tex8.location, 2);

    let mut seen = std::collections::HashSet::<u32>::new();
    for s in &translated.semantic_locations {
        assert!(
            seen.insert(s.location),
            "duplicate semantic location mapping: {s:?}"
        );
    }
}

#[test]
fn translate_sm3_vertex_input_semantic_locations_are_exposed() {
    let vs_bytes = to_bytes(&assemble_vs_passthrough_with_dcl_sm3_decoder());
    let translated =
        shader_translate::translate_d3d9_shader_to_wgsl(&vs_bytes, shader::WgslOptions::default())
            .unwrap();
    assert_eq!(
        translated.backend,
        shader_translate::ShaderTranslateBackend::Sm3
    );
    assert!(translated.uses_semantic_locations);

    // Ensure semantic-based remapping is surfaced for host-side executors (vertex attribute
    // bindings must match the WGSL @location mapping).
    assert!(translated
        .semantic_locations
        .contains(&shader::SemanticLocation {
            usage: crate::vertex::DeclUsage::Position,
            usage_index: 0,
            location: 0,
        }));
    assert!(translated
        .semantic_locations
        .contains(&shader::SemanticLocation {
            usage: crate::vertex::DeclUsage::TexCoord,
            usage_index: 0,
            location: 8,
        }));
    assert!(translated
        .semantic_locations
        .contains(&shader::SemanticLocation {
            usage: crate::vertex::DeclUsage::Color,
            usage_index: 0,
            location: 6,
        }));
}

#[test]
fn translate_entrypoint_falls_back_on_unsupported_opcode() {
    // Include an unknown opcode that the strict SM3 decoder rejects, but the legacy translator
    // skips (to support incremental bring-up).
    let ps_bytes = to_bytes(&assemble_ps_with_unknown_opcode());
    let translated =
        shader_translate::translate_d3d9_shader_to_wgsl(&ps_bytes, shader::WgslOptions::default())
            .unwrap();
    assert_eq!(
        translated.backend,
        shader_translate::ShaderTranslateBackend::LegacyFallback
    );
    assert!(translated
        .fallback_reason
        .as_deref()
        .unwrap_or("")
        .contains("unsupported"));

    let module = naga::front::wgsl::parse_str(&translated.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn translate_entrypoint_rejects_invalid_src_modifier_after_fallback() {
    // Ensure malformed token encodings are still reported as `Malformed` even when the shader
    // triggers SM3→legacy fallback (e.g. due to an unknown opcode).
    let mut words = vec![0xFFFF_0300];
    // Unknown opcode triggers fallback.
    words.extend(enc_inst(0x1234, &[]));
    // mov r0, c0 with an invalid source modifier (15)
    words.extend(enc_inst(
        0x0001,
        &[enc_dst(0, 0, 0xF), enc_src_mod(2, 0, 0xE4, 15)],
    ));
    words.push(0x0000_FFFF);

    let err = shader_translate::translate_d3d9_shader_to_wgsl(
        &to_bytes(&words),
        shader::WgslOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );
}

#[test]
fn translate_entrypoint_rejects_invalid_ifc_compare_op_after_fallback() {
    // Similar to the source-modifier test above: invalid `ifc` comparison op encodings are
    // malformed and should not be reported as a generic translation failure.
    let mut words = vec![0xFFFF_0300];
    // Unknown opcode triggers fallback.
    words.extend(enc_inst(0x1234, &[]));
    // ifc with invalid cmp code 7 (valid range is 0..=5)
    words.extend(enc_inst_with_extra(
        0x0029,
        7u32 << 16,
        &[enc_src(2, 0, 0xE4), enc_src(2, 1, 0xE4)],
    ));
    words.push(0x0000_FFFF);

    let err = shader_translate::translate_d3d9_shader_to_wgsl(
        &to_bytes(&words),
        shader::WgslOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );
}

#[test]
fn translate_entrypoint_rejects_out_of_range_register_index_after_fallback() {
    // Ensure register index limits are still enforced (and classified as malformed) even when the
    // shader triggers SM3→legacy fallback due to an unknown opcode.
    //
    // Note: The SM3 decoder currently allows larger indices for some output register files, while
    // the legacy parser applies tighter D3D9 caps. This test ensures those legacy-parser failures
    // are treated as `Malformed`, not a generic translation error.
    let mut words = vec![0xFFFE_0200]; // vs_2_0
                                       // Unknown opcode triggers fallback.
    words.extend(enc_inst(0x1234, &[]));
    // mov oD200, v0 (oD index exceeds Aero's supported range)
    words.extend(enc_inst(
        0x0001,
        &[enc_dst(5, 200, 0xF), enc_src(1, 0, 0xE4)],
    ));
    words.push(0x0000_FFFF);

    let err = shader_translate::translate_d3d9_shader_to_wgsl(
        &to_bytes(&words),
        shader::WgslOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );
}

#[test]
fn translate_entrypoint_rejects_sampler_operand_with_src_modifier_after_fallback() {
    // Ensure malformed sampler operand encodings are rejected even when the shader triggers
    // SM3→legacy fallback (e.g. due to an unknown opcode).
    let mut words = vec![0xFFFF_0300];
    // Unknown opcode triggers fallback.
    words.extend(enc_inst(0x1234, &[]));
    // texld r0, t0, -s0
    words.extend(enc_inst(
        0x0042,
        &[
            enc_dst(0, 0, 0xF),
            enc_src(3, 0, 0xE4),
            enc_src_mod(10, 0, 0xE4, 1), // negate
        ],
    ));
    words.push(0x0000_FFFF);

    let err = shader_translate::translate_d3d9_shader_to_wgsl(
        &to_bytes(&words),
        shader::WgslOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );
}

#[test]
fn translate_entrypoint_rejects_sampler_register_as_src_operand_after_fallback() {
    // Sampler registers (`s#`) are not numeric sources; reject even when legacy fallback runs.
    let mut words = vec![0xFFFF_0300];
    // Unknown opcode triggers fallback.
    words.extend(enc_inst(0x1234, &[]));
    // mov r0, s0
    words.extend(enc_inst(
        0x0001,
        &[enc_dst(0, 0, 0xF), enc_src(10, 0, 0xE4)],
    ));
    words.push(0x0000_FFFF);

    let err = shader_translate::translate_d3d9_shader_to_wgsl(
        &to_bytes(&words),
        shader::WgslOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );
}

#[test]
fn translate_entrypoint_rejects_input_register_as_dst_operand_after_fallback() {
    // Input registers (`v#`) are read-only at runtime; legacy fallback should not emit invalid WGSL
    // that assigns to them.
    let mut words = vec![0xFFFF_0300];
    // Unknown opcode triggers fallback.
    words.extend(enc_inst(0x1234, &[]));
    // mov v0, c0
    words.extend(enc_inst(0x0001, &[enc_dst(1, 0, 0xF), enc_src(2, 0, 0xE4)]));
    words.push(0x0000_FFFF);

    let err = shader_translate::translate_d3d9_shader_to_wgsl(
        &to_bytes(&words),
        shader::WgslOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );
}

#[test]
fn translate_entrypoint_rejects_pixel_shader_write_to_o_pos_after_fallback() {
    // Pixel shaders cannot write to oPos (`oPos` is a VS output). Legacy fallback would otherwise
    // generate invalid WGSL (missing variable declaration).
    let mut words = vec![0xFFFF_0300];
    // Unknown opcode triggers fallback.
    words.extend(enc_inst(0x1234, &[]));
    // mov oPos, c0
    words.extend(enc_inst(0x0001, &[enc_dst(4, 0, 0xF), enc_src(2, 0, 0xE4)]));
    words.push(0x0000_FFFF);

    let err = shader_translate::translate_d3d9_shader_to_wgsl(
        &to_bytes(&words),
        shader::WgslOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );
}

#[test]
fn translate_entrypoint_rejects_relative_addressing_on_dst_operand_after_fallback() {
    // Relative addressing on destination operands is malformed bytecode. Ensure it is still
    // rejected when legacy fallback runs.
    const RELATIVE: u32 = 0x0000_2000;

    let mut words = vec![0xFFFF_0300];
    // Unknown opcode triggers fallback.
    words.extend(enc_inst(0x1234, &[]));
    // mov r0[a0.x], c0
    let dst = enc_dst(0, 0, 0xF) | RELATIVE;
    let rel = enc_src(3, 0, 0x00); // a0.x
    words.extend(enc_inst(0x0001, &[dst, rel, enc_src(2, 0, 0xE4)]));
    words.push(0x0000_FFFF);

    let err = shader_translate::translate_d3d9_shader_to_wgsl(
        &to_bytes(&words),
        shader::WgslOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );
}

#[test]
fn translate_entrypoint_rejects_unknown_sampler_texture_type() {
    // An unknown `dcl_* s#` texture type encoding should be treated as malformed input (not a
    // fallbackable "unsupported feature").
    //
    // This triggers fallback via the SM3 WGSL generator error, then ensures the legacy parser's
    // sampler-type rejection is surfaced as `Malformed`.
    let mut words = vec![0xFFFF_0300]; // ps_3_0
                                       // dcl_<unknown> s0 (texture type 0 encoded in opcode_token[16..20])
    words.extend(enc_inst_with_extra(
        0x001F,
        0u32 << 16,
        &[enc_dst(10, 0, 0xF)],
    ));
    // texld r0, t0, s0
    words.extend(enc_inst(
        0x0042,
        &[
            enc_dst(0, 0, 0xF),
            enc_src(3, 0, 0xE4),
            enc_src(10, 0, 0xE4),
        ],
    ));
    // mov oC0, r0
    words.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    words.push(0x0000_FFFF);

    let err = shader_translate::translate_d3d9_shader_to_wgsl(
        &to_bytes(&words),
        shader::WgslOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );
}

#[test]
fn translate_entrypoint_rejects_invalid_tex_specific_field() {
    // The `tex` opcode (0x42) uses a 4-bit "specific" field in opcode_token[16..20] to select
    // between `texld` (0), `texldp` (1), and `texldb` (2). Other values are malformed input and
    // must not trigger legacy fallback.
    let mut words = vec![0xFFFF_0300]; // ps_3_0
                                       // dcl_2d s0
    words.extend(enc_inst_with_extra(
        0x001F,
        2u32 << 16,
        &[enc_dst(10, 0, 0xF)],
    ));
    // texld r0, t0, s0 with invalid specific field 3.
    words.extend(enc_inst_with_extra(
        0x0042,
        3u32 << 16,
        &[
            enc_dst(0, 0, 0xF),
            enc_src(3, 0, 0xE4),
            enc_src(10, 0, 0xE4),
        ],
    ));
    // mov oC0, r0
    words.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    words.push(0x0000_FFFF);

    let err = shader_translate::translate_d3d9_shader_to_wgsl(
        &to_bytes(&words),
        shader::WgslOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );
}

#[test]
fn translate_entrypoint_rejects_nested_relative_addressing() {
    // Craft a minimal ps_3_0 shader with nested relative addressing in a source operand.
    // Nested relative addressing is malformed SM2/SM3 bytecode and should be rejected as
    // `Malformed` (not treated as an "unsupported feature" that triggers legacy fallback).
    const RELATIVE: u32 = 0x0000_2000;
    let src = enc_src(2, 0, 0xE4) | RELATIVE; // c0[...]
    let rel = enc_src(3, 0, 0xE4) | RELATIVE; // a0.x with RELATIVE bit set -> nested relative
    let mut words = vec![0xFFFF_0300];
    words.extend(enc_inst(
        0x0001, // mov
        &[enc_dst(0, 0, 0xF), src, rel],
    ));
    words.push(0x0000_FFFF);

    let err = shader_translate::translate_d3d9_shader_to_wgsl(
        &to_bytes(&words),
        shader::WgslOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );
}

#[test]
fn translate_entrypoint_rejects_invalid_predicate_modifier() {
    // Predicated instructions append a predicate register token. Predicate register source
    // modifiers other than None/Negate are malformed and should not trigger fallback.
    let mut words = vec![0xFFFF_0300];
    words.extend(enc_inst_with_extra(
        0x0001,      // mov
        0x1000_0000, // predicated flag
        &[
            enc_dst(8, 0, 0xF),          // oC0
            enc_src(2, 0, 0xE4),         // c0
            enc_src_mod(19, 0, 0xE4, 2), // p0 with invalid modifier (bias)
        ],
    ));
    words.push(0x0000_FFFF);

    let err = shader_translate::translate_d3d9_shader_to_wgsl(
        &to_bytes(&words),
        shader::WgslOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );
}

#[test]
fn translate_entrypoint_rejects_invalid_src_modifier() {
    // Source modifier values outside the D3D9 SM2/SM3 enum range are malformed input and should not
    // trigger legacy fallback.
    let mut words = vec![0xFFFF_0300];
    // mov r0, c0 with an invalid source modifier (15)
    words.extend(enc_inst(
        0x0001,
        &[enc_dst(0, 0, 0xF), enc_src_mod(2, 0, 0xE4, 15)],
    ));
    words.push(0x0000_FFFF);

    let err = shader_translate::translate_d3d9_shader_to_wgsl(
        &to_bytes(&words),
        shader::WgslOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );
}

#[test]
fn translate_entrypoint_rejects_relative_register_addressing() {
    // Craft a ps_3_0 shader that uses relative addressing on a non-constant register (r1[a0.x]).
    // This currently fails during WGSL lowering, and should be treated as malformed input (not as
    // an "unsupported feature" that triggers legacy fallback).
    const RELATIVE: u32 = 0x0000_2000;

    let mut words = vec![0xFFFF_0300];
    // mov r0, r1[a0.x]
    let src = enc_src(0, 1, 0xE4) | RELATIVE; // r1[...]
    let rel = enc_src(3, 0, 0x00); // a0.x (swizzle = xxxx)
    words.extend(enc_inst(0x0001, &[enc_dst(0, 0, 0xF), src, rel]));
    words.push(0x0000_FFFF);

    let err = shader_translate::translate_d3d9_shader_to_wgsl(
        &to_bytes(&words),
        shader::WgslOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );
}

#[test]
fn translate_entrypoint_rejects_relative_label_addressing() {
    // Relative addressing on label (`l#`) operands is malformed bytecode. Without special-casing,
    // the IR builder error message contained "not supported" and would trigger SM3→legacy fallback.
    // Ensure it is rejected as `Malformed` instead of using fallback as an escape hatch.
    const RELATIVE: u32 = 0x0000_2000;

    let mut words = vec![0xFFFF_0300];
    // mov r0, c0
    words.extend(enc_inst(0x0001, &[enc_dst(0, 0, 0xF), enc_src(2, 0, 0xE4)]));
    // call l0[a0.x] (nonsense; label operands are not addressable)
    let label = enc_src(18, 0, 0xE4) | RELATIVE;
    let rel = enc_src(3, 0, 0x00); // a0.x
    words.extend(enc_inst(0x0019, &[label, rel]));
    // mov oC0, r0
    words.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    words.push(0x0000_FFFF);

    let err = shader_translate::translate_d3d9_shader_to_wgsl(
        &to_bytes(&words),
        shader::WgslOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );
}

#[test]
fn translate_entrypoint_rejects_const_register_as_dst_operand() {
    // Constant registers (`c#`) are read-only at runtime. Writing to them would produce invalid
    // WGSL (no mutable storage for `c#`), so treat it as malformed bytecode.
    let mut words = vec![0xFFFF_0300];
    // mov c0, r0
    words.extend(enc_inst(0x0001, &[enc_dst(2, 0, 0xF), enc_src(0, 0, 0xE4)]));
    words.push(0x0000_FFFF);

    let err = shader_translate::translate_d3d9_shader_to_wgsl(
        &to_bytes(&words),
        shader::WgslOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );
}

#[test]
fn translate_entrypoint_rejects_input_register_as_dst_operand() {
    // Input registers (`v#`) are read-only at runtime.
    let mut words = vec![0xFFFF_0300];
    // mov v0, c0
    words.extend(enc_inst(0x0001, &[enc_dst(1, 0, 0xF), enc_src(2, 0, 0xE4)]));
    words.push(0x0000_FFFF);

    let err = shader_translate::translate_d3d9_shader_to_wgsl(
        &to_bytes(&words),
        shader::WgslOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );
}

#[test]
fn translate_entrypoint_rejects_texture_register_as_dst_operand() {
    // Texture input registers (`t#`) are read-only at runtime in SM2/SM3.
    let mut words = vec![0xFFFF_0300];
    // mov t0, c0
    words.extend(enc_inst(0x0001, &[enc_dst(3, 0, 0xF), enc_src(2, 0, 0xE4)]));
    words.push(0x0000_FFFF);

    let err = shader_translate::translate_d3d9_shader_to_wgsl(
        &to_bytes(&words),
        shader::WgslOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );
}

#[test]
fn translate_entrypoint_rejects_misctype_register_as_dst_operand() {
    // MISCTYPE (`misc#`) inputs are read-only at runtime.
    let mut words = vec![0xFFFF_0300];
    // mov misc0, c0
    words.extend(enc_inst(
        0x0001,
        &[enc_dst(17, 0, 0xF), enc_src(2, 0, 0xE4)],
    ));
    words.push(0x0000_FFFF);

    let err = shader_translate::translate_d3d9_shader_to_wgsl(
        &to_bytes(&words),
        shader::WgslOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );
}

#[test]
fn translate_entrypoint_rejects_loop_register_as_dst_operand() {
    // Loop registers (`aL#`) are not writable by general ops; they are updated implicitly by
    // `loop`/`rep`.
    let mut words = vec![0xFFFF_0300];
    // mov aL, aL
    words.extend(enc_inst(
        0x0001,
        &[enc_dst(15, 0, 0xF), enc_src(15, 0, 0xE4)],
    ));
    words.push(0x0000_FFFF);

    let err = shader_translate::translate_d3d9_shader_to_wgsl(
        &to_bytes(&words),
        shader::WgslOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );
}

#[test]
fn translate_entrypoint_rejects_predicate_register_as_dst_operand() {
    // Predicate registers (`p#`) are only writable via `setp` and should not be writable by generic
    // ops like `mov`.
    let mut words = vec![0xFFFF_0300];
    // mov p0, p0
    words.extend(enc_inst(
        0x0001,
        &[enc_dst(19, 0, 0x1), enc_src(19, 0, 0xE4)],
    ));
    words.push(0x0000_FFFF);

    let err = shader_translate::translate_d3d9_shader_to_wgsl(
        &to_bytes(&words),
        shader::WgslOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );
}

#[test]
fn translate_entrypoint_rejects_addr_register_write_with_mov() {
    // Address registers (`a#`) are only writable via `mova` in D3D9.
    let mut words = vec![0xFFFE_0300]; // vs_3_0
                                       // mov a0.x, a0.x
    words.extend(enc_inst(0x0001, &[enc_dst(3, 0, 0x1), enc_src(3, 0, 0xE4)]));
    // mov oPos, v0 (otherwise-valid VS output)
    words.extend(enc_inst(0x0001, &[enc_dst(4, 0, 0xF), enc_src(1, 0, 0xE4)]));
    words.push(0x0000_FFFF);

    let err = shader_translate::translate_d3d9_shader_to_wgsl(
        &to_bytes(&words),
        shader::WgslOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );
}

#[test]
fn translate_entrypoint_rejects_relative_addressing_on_dst_operand() {
    // Relative addressing on destination operands is invalid in D3D9 SM2/SM3. Reject it as
    // malformed rather than attempting legacy fallback.
    const RELATIVE: u32 = 0x0000_2000;

    let mut words = vec![0xFFFF_0300];
    // mov r0[a0.x], c0
    let dst = enc_dst(0, 0, 0xF) | RELATIVE;
    let rel = enc_src(3, 0, 0x00); // a0.x
    words.extend(enc_inst(0x0001, &[dst, rel, enc_src(2, 0, 0xE4)]));
    words.push(0x0000_FFFF);

    let err = shader_translate::translate_d3d9_shader_to_wgsl(
        &to_bytes(&words),
        shader::WgslOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );
}

#[test]
fn translate_entrypoint_rejects_pixel_shader_write_to_o_pos() {
    // Pixel shaders cannot write vertex raster outputs like `oPos`.
    let mut words = vec![0xFFFF_0300];
    // mov oPos, c0
    words.extend(enc_inst(0x0001, &[enc_dst(4, 0, 0xF), enc_src(2, 0, 0xE4)]));
    words.push(0x0000_FFFF);

    let err = shader_translate::translate_d3d9_shader_to_wgsl(
        &to_bytes(&words),
        shader::WgslOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );
}

#[test]
fn translate_entrypoint_rejects_vertex_shader_write_to_o_depth() {
    // Vertex shaders cannot write pixel shader depth outputs (`oDepth`).
    let mut words = vec![0xFFFE_0300]; // vs_3_0
                                       // mov oDepth, v0
    words.extend(enc_inst(0x0001, &[enc_dst(9, 0, 0xF), enc_src(1, 0, 0xE4)]));
    words.push(0x0000_FFFF);

    let err = shader_translate::translate_d3d9_shader_to_wgsl(
        &to_bytes(&words),
        shader::WgslOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );
}

#[test]
fn translate_entrypoint_rejects_relative_sampler_addressing() {
    // Sampler operands are not addressable. Relative addressing on sampler operands should be
    // treated as malformed input and rejected by the strict SM3 pipeline (not silently ignored).
    const RELATIVE: u32 = 0x0000_2000;

    let mut words = vec![0xFFFF_0300];
    // texld r0, t0, s0[a0.x]
    let sampler = enc_src(10, 0, 0xE4) | RELATIVE;
    let rel = enc_src(3, 0, 0x00); // a0.x (swizzle = xxxx)
    words.extend(enc_inst(
        0x0042,
        &[enc_dst(0, 0, 0xF), enc_src(3, 0, 0xE4), sampler, rel],
    ));
    // mov oC0, r0
    words.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    words.push(0x0000_FFFF);

    let err = shader_translate::translate_d3d9_shader_to_wgsl(
        &to_bytes(&words),
        shader::WgslOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );
}

#[test]
fn translate_entrypoint_rejects_sampler_operand_with_src_modifier() {
    // Sampler operands are not general-purpose source operands; they should not carry source
    // modifiers. Reject as malformed to avoid ignoring invalid encodings.
    let mut words = vec![0xFFFF_0300];
    // texld r0, t0, -s0
    words.extend(enc_inst(
        0x0042,
        &[
            enc_dst(0, 0, 0xF),
            enc_src(3, 0, 0xE4),
            enc_src_mod(10, 0, 0xE4, 1), // negate
        ],
    ));
    // mov oC0, r0
    words.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    words.push(0x0000_FFFF);

    let err = shader_translate::translate_d3d9_shader_to_wgsl(
        &to_bytes(&words),
        shader::WgslOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );
}

#[test]
fn translate_entrypoint_rejects_relative_def_destination_operand() {
    // `def` destinations are not addressable; reject malformed relative-addressing encodings.
    const RELATIVE: u32 = 0x0000_2000;

    let mut words = vec![0xFFFF_0300];
    // def c0[a0.x], 1.0, 0.0, 0.0, 1.0
    let dst = enc_dst(2, 0, 0xF) | RELATIVE;
    let rel = enc_src(3, 0, 0x00); // a0.x
    words.extend(enc_inst(
        0x0051,
        &[dst, rel, 0x3F80_0000, 0x0000_0000, 0x0000_0000, 0x3F80_0000],
    ));
    // mov oC0, c0
    words.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(2, 0, 0xE4)]));
    words.push(0x0000_FFFF);

    let err = shader_translate::translate_d3d9_shader_to_wgsl(
        &to_bytes(&words),
        shader::WgslOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );
}

#[test]
fn translate_entrypoint_rejects_relative_dcl_destination_operand() {
    // `dcl` destinations are not addressable; reject malformed relative-addressing encodings.
    const RELATIVE: u32 = 0x0000_2000;

    let mut words = vec![0xFFFF_0300];
    // Legacy DCL form: <decl_token>, <dst>
    // dcl_position v0[a0.x]
    let decl_token = 0u32; // usage = position (0), usage_index = 0
    let dst = enc_dst(1, 0, 0xF) | RELATIVE;
    let rel = enc_src(3, 0, 0x00); // a0.x
    words.extend(enc_inst(0x001F, &[decl_token, dst, rel]));
    words.push(0x0000_FFFF);

    let err = shader_translate::translate_d3d9_shader_to_wgsl(
        &to_bytes(&words),
        shader::WgslOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );
}

#[test]
fn translate_entrypoint_rejects_label_register_as_src_operand() {
    // Label registers (`l#`) are not runtime storage and may not appear as generic source operands.
    let mut words = vec![0xFFFF_0300];
    // mov r0, l0
    words.extend(enc_inst(
        0x0001,
        &[enc_dst(0, 0, 0xF), enc_src(18, 0, 0xE4)],
    ));
    words.push(0x0000_FFFF);

    let err = shader_translate::translate_d3d9_shader_to_wgsl(
        &to_bytes(&words),
        shader::WgslOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );
}

#[test]
fn translate_entrypoint_rejects_sampler_register_as_src_operand() {
    // Sampler registers (`s#`) are not general-purpose source operands. They should only appear as
    // the sampler argument to texture sampling instructions (`texld`, `texldd`, etc).
    let mut words = vec![0xFFFF_0300];
    // mov oC0, s0
    words.extend(enc_inst(
        0x0001,
        &[enc_dst(8, 0, 0xF), enc_src(10, 0, 0xE4)],
    ));
    words.push(0x0000_FFFF);

    let err = shader_translate::translate_d3d9_shader_to_wgsl(
        &to_bytes(&words),
        shader::WgslOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );
}

#[test]
fn translate_entrypoint_rejects_sampler_register_as_dst_operand() {
    // Writing to sampler registers is invalid and should be rejected as malformed.
    let mut words = vec![0xFFFF_0300];
    // mov s0, c0
    words.extend(enc_inst(
        0x0001,
        &[enc_dst(10, 0, 0xF), enc_src(2, 0, 0xE4)],
    ));
    words.push(0x0000_FFFF);

    let err = shader_translate::translate_d3d9_shader_to_wgsl(
        &to_bytes(&words),
        shader::WgslOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );
}

#[test]
fn translate_entrypoint_rejects_setp_with_result_modifiers() {
    // `setp` writes predicate registers. Saturate/shift modifiers are meaningless and should be
    // rejected as malformed instead of triggering legacy fallback.
    let mut words = vec![0xFFFF_0300];
    // setp_sat_ge p0, c0, c1  (cmp code 2 = ge, saturate flag set)
    words.extend(enc_inst_with_extra(
        0x005E,
        (2u32 << 16) | (1u32 << 20),
        &[
            enc_dst(19, 0, 0x1),
            enc_src(2, 0, 0xE4),
            enc_src(2, 1, 0xE4),
        ],
    ));
    words.push(0x0000_FFFF);

    let err = shader_translate::translate_d3d9_shader_to_wgsl(
        &to_bytes(&words),
        shader::WgslOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );
}

#[test]
fn translate_entrypoint_legacy_fallback_supports_derivatives() {
    // Ensure the legacy fallback translator implements `dsx`/`dsy` so shaders that fall back due
    // to unrelated SM3-pipeline limitations can still compute derivatives.
    let ps_bytes = to_bytes(&assemble_ps_with_unknown_opcode_and_derivatives());
    let translated =
        shader_translate::translate_d3d9_shader_to_wgsl(&ps_bytes, shader::WgslOptions::default())
            .unwrap();
    assert_eq!(
        translated.backend,
        shader_translate::ShaderTranslateBackend::LegacyFallback
    );
    assert!(translated.wgsl.contains("dpdx("));
    assert!(translated.wgsl.contains("dpdy("));

    let module = naga::front::wgsl::parse_str(&translated.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn translate_entrypoint_rejects_truncated_token_stream() {
    let mut bytes = to_bytes(&assemble_vs_passthrough_sm3_decoder());
    // Drop the END token and one operand token from the last instruction, leaving a truncated
    // instruction stream.
    bytes.truncate(bytes.len().saturating_sub(8));
    let err =
        shader_translate::translate_d3d9_shader_to_wgsl(&bytes, shader::WgslOptions::default())
            .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );
}

#[test]
fn shader_cache_dedupes_by_hash() {
    let vs_bytes = to_bytes(&assemble_vs_passthrough());
    let dxbc = dxbc_test_utils::build_container(&[(DxbcFourCC(*b"SHDR"), &vs_bytes)]);

    let mut cache = shader::ShaderCache::default();
    let a = cache.get_or_translate(&dxbc).unwrap().hash;
    let b = cache.get_or_translate(&dxbc).unwrap().hash;
    assert_eq!(a, b);
}

#[test]
fn state_defaults_are_stable() {
    let blend = state::BlendState::default();
    assert_eq!(blend.enabled, false);

    let depth = state::DepthState::default();
    assert_eq!(depth.enabled, false);

    let raster = state::RasterState::default();
    assert_eq!(raster.cull, state::CullMode::Back);
}

#[test]
fn translates_simple_ps_to_wgsl() {
    let ps_bytes = to_bytes(&assemble_ps_texture_modulate());
    let dxbc = dxbc_test_utils::build_container(&[(DxbcFourCC(*b"SHDR"), &ps_bytes)]);
    let program = shader::parse(&dxbc).unwrap();
    let ir = shader::to_ir(&program);
    let wgsl = shader::generate_wgsl(&ir).unwrap();

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(wgsl.wgsl.contains("@fragment"));
    assert!(wgsl.wgsl.contains("textureSample"));
}

#[test]
fn legacy_translator_nonuniform_if_dsx_avoids_invalid_control_flow() {
    let ps_bytes = to_bytes(&assemble_ps3_nonuniform_if_dsx());
    let program = shader::parse(&ps_bytes).unwrap();
    let ir = shader::to_ir(&program);
    let wgsl = shader::generate_wgsl(&ir).unwrap();

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(wgsl.wgsl.contains("dpdx("));
    // Branch should be lowered to `select` so that `dpdx` is evaluated unconditionally.
    assert!(!wgsl.wgsl.contains("if ("));
}

#[test]
fn legacy_translator_nonuniform_if_dsy_avoids_invalid_control_flow() {
    let ps_bytes = to_bytes(&assemble_ps3_nonuniform_if_dsy());
    let program = shader::parse(&ps_bytes).unwrap();
    let ir = shader::to_ir(&program);
    let wgsl = shader::generate_wgsl(&ir).unwrap();

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(wgsl.wgsl.contains("dpdy("));
    assert!(!wgsl.wgsl.contains("if ("));
}

#[test]
fn legacy_translator_nonuniform_ifc_dsx_avoids_invalid_control_flow() {
    let ps_bytes = to_bytes(&assemble_ps3_nonuniform_ifc_dsx());
    let program = shader::parse(&ps_bytes).unwrap();
    let ir = shader::to_ir(&program);
    let wgsl = shader::generate_wgsl(&ir).unwrap();

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(wgsl.wgsl.contains("dpdx("));
    assert!(!wgsl.wgsl.contains("if ("));
}

#[test]
fn legacy_translator_nonuniform_if_texld_avoids_invalid_control_flow() {
    let ps_bytes = to_bytes(&assemble_ps2_nonuniform_if_texld());
    let program = shader::parse(&ps_bytes).unwrap();
    let ir = shader::to_ir(&program);
    let wgsl = shader::generate_wgsl(&ir).unwrap();

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(wgsl.wgsl.contains("textureSample("));
    // Branch should be lowered to `select` so that `textureSample` is evaluated unconditionally.
    assert!(!wgsl.wgsl.contains("if ("));
}

#[test]
fn legacy_translator_nonuniform_if_dsx_then_mov_is_naga_valid() {
    let ps_bytes = to_bytes(&assemble_ps3_nonuniform_if_dsx_then_mov());
    let program = shader::parse(&ps_bytes).unwrap();
    let ir = shader::to_ir(&program);
    let wgsl = shader::generate_wgsl(&ir).unwrap();

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
    assert!(wgsl.wgsl.contains("dpdx("));
}

#[test]
fn legacy_translator_nonuniform_if_mov_then_dsx_is_naga_valid() {
    let ps_bytes = to_bytes(&assemble_ps3_nonuniform_if_mov_then_dsx());
    let program = shader::parse(&ps_bytes).unwrap();
    let ir = shader::to_ir(&program);
    let wgsl = shader::generate_wgsl(&ir).unwrap();

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(wgsl.wgsl.contains("dpdx("));
    assert!(!wgsl.wgsl.contains("if ("));
}

#[test]
fn legacy_translator_nonuniform_if_nested_dsx_is_naga_valid() {
    let ps_bytes = to_bytes(&assemble_ps3_nonuniform_if_nested_dsx());
    let program = shader::parse(&ps_bytes).unwrap();
    let ir = shader::to_ir(&program);
    let wgsl = shader::generate_wgsl(&ir).unwrap();

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(wgsl.wgsl.contains("dpdx("));
    assert!(!wgsl.wgsl.contains("if ("));
}

#[test]
fn legacy_translator_nonuniform_if_texld_then_mov_is_naga_valid() {
    let ps_bytes = to_bytes(&assemble_ps2_nonuniform_if_texld_then_mov());
    let program = shader::parse(&ps_bytes).unwrap();
    let ir = shader::to_ir(&program);
    let wgsl = shader::generate_wgsl(&ir).unwrap();

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
    assert!(wgsl.wgsl.contains("textureSample("));
}

#[test]
fn legacy_translator_nonuniform_if_mov_then_texld_is_naga_valid() {
    let ps_bytes = to_bytes(&assemble_ps2_nonuniform_if_mov_then_texld());
    let program = shader::parse(&ps_bytes).unwrap();
    let ir = shader::to_ir(&program);
    let wgsl = shader::generate_wgsl(&ir).unwrap();

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(wgsl.wgsl.contains("textureSample("));
    assert!(!wgsl.wgsl.contains("if ("));
}

#[test]
fn legacy_translator_nonuniform_if_nested_texld_is_naga_valid() {
    let ps_bytes = to_bytes(&assemble_ps2_nonuniform_if_nested_texld());
    let program = shader::parse(&ps_bytes).unwrap();
    let ir = shader::to_ir(&program);
    let wgsl = shader::generate_wgsl(&ir).unwrap();

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(wgsl.wgsl.contains("textureSample("));
    assert!(!wgsl.wgsl.contains("if ("));
}

#[test]
fn legacy_translator_nonuniform_if_else_dsx_hoists_dpdx() {
    let ps_bytes = to_bytes(&assemble_ps3_nonuniform_if_else_dsx());
    let program = shader::parse(&ps_bytes).unwrap();
    let ir = shader::to_ir(&program);
    let wgsl = shader::generate_wgsl(&ir).unwrap();

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    let dpdx_pos = wgsl.wgsl.find("dpdx(").expect("dpdx present");
    let if_pos = wgsl.wgsl.find("if (").expect("if present");
    assert!(dpdx_pos < if_pos, "wgsl:\n{}", wgsl.wgsl);
}

#[test]
fn legacy_translator_nonuniform_if_else_dsx_then_mov_is_naga_valid() {
    let ps_bytes = to_bytes(&assemble_ps3_nonuniform_if_else_dsx_then_mov());
    let program = shader::parse(&ps_bytes).unwrap();
    let ir = shader::to_ir(&program);
    let wgsl = shader::generate_wgsl(&ir).unwrap();

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    let dpdx_pos = wgsl.wgsl.find("dpdx(").expect("dpdx present");
    let if_pos = wgsl.wgsl.find("if (").expect("if present");
    assert!(dpdx_pos < if_pos, "wgsl:\n{}", wgsl.wgsl);
}

#[test]
fn legacy_translator_nonuniform_if_else_mov_then_dsx_is_naga_valid() {
    let ps_bytes = to_bytes(&assemble_ps3_nonuniform_if_else_mov_then_dsx());
    let program = shader::parse(&ps_bytes).unwrap();
    let ir = shader::to_ir(&program);
    let wgsl = shader::generate_wgsl(&ir).unwrap();

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(wgsl.wgsl.contains("dpdx("));
    assert!(!wgsl.wgsl.contains("if ("));
}

#[test]
fn legacy_translator_nonuniform_if_else_texld_hoists_texture_sample() {
    let ps_bytes = to_bytes(&assemble_ps2_nonuniform_if_else_texld());
    let program = shader::parse(&ps_bytes).unwrap();
    let ir = shader::to_ir(&program);
    let wgsl = shader::generate_wgsl(&ir).unwrap();

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    let sample_pos = wgsl
        .wgsl
        .find("textureSample(")
        .expect("textureSample present");
    let if_pos = wgsl.wgsl.find("if (").expect("if present");
    assert!(sample_pos < if_pos, "wgsl:\n{}", wgsl.wgsl);
}

#[test]
fn legacy_translator_nonuniform_if_else_texld_then_mov_is_naga_valid() {
    let ps_bytes = to_bytes(&assemble_ps2_nonuniform_if_else_texld_then_mov());
    let program = shader::parse(&ps_bytes).unwrap();
    let ir = shader::to_ir(&program);
    let wgsl = shader::generate_wgsl(&ir).unwrap();

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    let sample_pos = wgsl
        .wgsl
        .find("textureSample(")
        .expect("textureSample present");
    let if_pos = wgsl.wgsl.find("if (").expect("if present");
    assert!(sample_pos < if_pos, "wgsl:\n{}", wgsl.wgsl);
}

#[test]
fn legacy_translator_nonuniform_if_else_mov_then_texld_is_naga_valid() {
    let ps_bytes = to_bytes(&assemble_ps2_nonuniform_if_else_mov_then_texld());
    let program = shader::parse(&ps_bytes).unwrap();
    let ir = shader::to_ir(&program);
    let wgsl = shader::generate_wgsl(&ir).unwrap();

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(wgsl.wgsl.contains("textureSample("));
    assert!(!wgsl.wgsl.contains("if ("));
}

#[test]
fn translates_additional_ps_ops_to_wgsl() {
    let ps_bytes = to_bytes(&assemble_ps_math_ops());
    let program = shader::parse(&ps_bytes).unwrap();
    let ir = shader::to_ir(&program);
    let wgsl = shader::generate_wgsl(&ir).unwrap();

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(wgsl.wgsl.contains("min("));
    assert!(wgsl.wgsl.contains("max("));
    assert!(wgsl.wgsl.contains("inverseSqrt"));
    assert!(wgsl.wgsl.contains("fract("));
    assert!(wgsl.wgsl.contains("exp2("));
    assert!(wgsl.wgsl.contains("log2("));
    assert!(wgsl.wgsl.contains("pow("));
    assert!(wgsl.wgsl.contains("select("));
    assert!(wgsl.wgsl.contains("dot(("));
    assert!(wgsl.wgsl.contains(").xy"));
}

#[test]
fn translates_ps2_dp2_masked_write_to_wgsl() {
    let ps_bytes = to_bytes(&assemble_ps2_dp2_masked_xy());
    let program = shader::parse(&ps_bytes).unwrap();
    let ir = shader::to_ir(&program);
    let wgsl = shader::generate_wgsl(&ir).unwrap();

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(wgsl.wgsl.contains("dot("), "wgsl:\n{}", wgsl.wgsl);
}

#[test]
fn translates_ps3_ifc_def_to_wgsl() {
    let ps_bytes = to_bytes(&assemble_ps3_tex_ifc_def());
    let program = shader::parse(&ps_bytes).unwrap();
    let ir = shader::to_ir(&program);
    let wgsl = shader::generate_wgsl(&ir).unwrap();

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(wgsl.wgsl.contains("if ("));
    assert!(wgsl.wgsl.contains("} else {"));
    assert!(wgsl.wgsl.contains("let c0: vec4<f32>"));
}

#[test]
fn translates_ps3_lrp_to_wgsl() {
    let ps_bytes = to_bytes(&assemble_ps3_lrp());
    let program = shader::parse(&ps_bytes).unwrap();
    let ir = shader::to_ir(&program);
    let wgsl = shader::generate_wgsl(&ir).unwrap();

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(wgsl.wgsl.contains("mix("), "wgsl:\n{}", wgsl.wgsl);
}

#[test]
fn sm3_translates_predicated_lrp_to_wgsl() {
    let ps_tokens = assemble_ps3_predicated_lrp();
    let decoded = crate::sm3::decode_u32_tokens(&ps_tokens).unwrap();
    let ir = crate::sm3::build_ir(&decoded).unwrap();
    crate::sm3::verify_ir(&ir).unwrap();
    let wgsl = crate::sm3::generate_wgsl(&ir).unwrap();

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(wgsl.wgsl.contains("mix("), "wgsl:\n{}", wgsl.wgsl);
    assert!(wgsl.wgsl.contains("clamp("), "wgsl:\n{}", wgsl.wgsl);
    assert!(wgsl.wgsl.contains("* 2.0"), "wgsl:\n{}", wgsl.wgsl);
    assert!(wgsl.wgsl.contains("if (p0.x)"), "wgsl:\n{}", wgsl.wgsl);
}

#[test]
fn translates_ps_mrt_outputs_to_wgsl() {
    let ps_bytes = to_bytes(&assemble_ps_mrt_solid_color());
    let program = shader::parse(&ps_bytes).unwrap();
    let ir = shader::to_ir(&program);
    let wgsl = shader::generate_wgsl(&ir).unwrap();

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(wgsl.wgsl.contains("@location(1) oC1"));
}

fn build_vertex_decl_pos_tex_color() -> state::VertexDecl {
    state::VertexDecl::new(
        40,
        vec![
            state::VertexElement {
                offset: 0,
                ty: state::VertexElementType::Float4,
                usage: state::VertexUsage::Position,
                usage_index: 0,
            },
            state::VertexElement {
                offset: 16,
                ty: state::VertexElementType::Float2,
                usage: state::VertexUsage::TexCoord,
                usage_index: 0,
            },
            state::VertexElement {
                offset: 24,
                ty: state::VertexElementType::Float4,
                usage: state::VertexUsage::Color,
                usage_index: 0,
            },
        ],
    )
}

fn build_vertex_decl_pos_tex4_color() -> state::VertexDecl {
    state::VertexDecl::new(
        48,
        vec![
            state::VertexElement {
                offset: 0,
                ty: state::VertexElementType::Float4,
                usage: state::VertexUsage::Position,
                usage_index: 0,
            },
            state::VertexElement {
                offset: 16,
                ty: state::VertexElementType::Float4,
                usage: state::VertexUsage::TexCoord,
                usage_index: 0,
            },
            state::VertexElement {
                offset: 32,
                ty: state::VertexElementType::Float4,
                usage: state::VertexUsage::Color,
                usage_index: 0,
            },
        ],
    )
}

fn push_f32(out: &mut Vec<u8>, v: f32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_vec4(out: &mut Vec<u8>, v: software::Vec4) {
    push_f32(out, v.x);
    push_f32(out, v.y);
    push_f32(out, v.z);
    push_f32(out, v.w);
}

fn push_vec2(out: &mut Vec<u8>, x: f32, y: f32) {
    push_f32(out, x);
    push_f32(out, y);
}

fn zero_constants() -> [software::Vec4; 256] {
    [software::Vec4::ZERO; 256]
}

#[test]
fn micro_triangle_solid_color_pixel_compare() {
    let vs = shader::to_ir(&shader::parse(&to_bytes(&assemble_vs_passthrough())).unwrap());
    let ps = shader::to_ir(&shader::parse(&to_bytes(&assemble_ps_color_passthrough())).unwrap());

    let decl = build_vertex_decl_pos_tex_color();

    let mut vb = Vec::new();
    let red = software::Vec4::new(1.0, 0.0, 0.0, 1.0);

    for (pos_x, pos_y) in [(-0.5, -0.5), (0.5, -0.5), (0.0, 0.5)] {
        push_vec4(&mut vb, software::Vec4::new(pos_x, pos_y, 0.0, 1.0));
        push_vec2(&mut vb, 0.0, 0.0);
        push_vec4(&mut vb, red);
    }

    let mut rt = software::RenderTarget::new(16, 16, software::Vec4::ZERO);
    let constants = zero_constants();
    let textures = HashMap::new();
    let sampler_states = HashMap::new();
    software::draw(
        &mut rt,
        software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: None,
            constants: &constants,
            textures: &textures,
            sampler_states: &sampler_states,
            blend_state: state::BlendState::default(),
        },
    );

    let rgba = rt.to_rgba8();
    let hash = blake3::hash(&rgba);
    // Stable output signature for regression testing.
    assert_eq!(
        hash.to_hex().as_str(),
        "f319f67af7e26fb3e108840dfe953de674f251a9542b12738334ad592fbff483"
    );
    assert_eq!(rt.get(8, 8).to_rgba8(), [255, 0, 0, 255]);
}

#[test]
fn micro_textured_quad_pixel_compare() {
    let vs = shader::to_ir(&shader::parse(&to_bytes(&assemble_vs_passthrough())).unwrap());
    let ps = shader::to_ir(&shader::parse(&to_bytes(&assemble_ps_texture_modulate())).unwrap());

    let decl = build_vertex_decl_pos_tex_color();

    let mut vb = Vec::new();
    let white = software::Vec4::new(1.0, 1.0, 1.0, 1.0);

    let verts = [
        (software::Vec4::new(-1.0, -1.0, 0.0, 1.0), (0.0, 1.0)), // bottom-left
        (software::Vec4::new(1.0, -1.0, 0.0, 1.0), (1.0, 1.0)),  // bottom-right
        (software::Vec4::new(1.0, 1.0, 0.0, 1.0), (1.0, 0.0)),   // top-right
        (software::Vec4::new(-1.0, 1.0, 0.0, 1.0), (0.0, 0.0)),  // top-left
    ];
    for (pos, (u, v)) in verts {
        push_vec4(&mut vb, pos);
        push_vec2(&mut vb, u, v);
        push_vec4(&mut vb, white);
    }

    let indices: [u16; 6] = [0, 1, 2, 0, 2, 3];

    let tex_bytes: [u8; 16] = [
        255, 0, 0, 255, // red (top-left)
        0, 255, 0, 255, // green (top-right)
        0, 0, 255, 255, // blue (bottom-left)
        255, 255, 255, 255, // white (bottom-right)
    ];
    let tex = software::Texture2D::from_rgba8(2, 2, &tex_bytes);

    let mut textures = HashMap::new();
    textures.insert(0u16, tex.into());

    let mut sampler_states = HashMap::new();
    sampler_states.insert(
        0u16,
        state::SamplerState {
            min_filter: state::FilterMode::Point,
            mag_filter: state::FilterMode::Point,
            address_u: state::AddressMode::Clamp,
            address_v: state::AddressMode::Clamp,
        },
    );

    let mut rt = software::RenderTarget::new(8, 8, software::Vec4::ZERO);
    let constants = zero_constants();
    software::draw(
        &mut rt,
        software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: Some(&indices),
            constants: &constants,
            textures: &textures,
            sampler_states: &sampler_states,
            blend_state: state::BlendState::default(),
        },
    );

    assert_eq!(rt.get(1, 1).to_rgba8(), [255, 0, 0, 255]); // top-left
    assert_eq!(rt.get(6, 1).to_rgba8(), [0, 255, 0, 255]); // top-right
    assert_eq!(rt.get(1, 6).to_rgba8(), [0, 0, 255, 255]); // bottom-left
    assert_eq!(rt.get(6, 6).to_rgba8(), [255, 255, 255, 255]); // bottom-right

    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "6fa50059441133e99a2414be50f613190809d5373953a6e414c373be772438f7"
    );
}

#[test]
fn micro_ps3_dcl_position_quadrants_pixel_compare() {
    let vs = build_sm3_ir(&assemble_vs_passthrough_sm3());
    let ps = build_sm3_ir(&assemble_ps3_dcl_position_quadrants_sm3_decoder());

    let decl = build_vertex_decl_pos_tex_color();

    let mut vb = Vec::new();
    let white = software::Vec4::new(1.0, 1.0, 1.0, 1.0);

    let verts = [
        (software::Vec4::new(-1.0, -1.0, 0.0, 1.0), (0.0, 1.0)), // bottom-left
        (software::Vec4::new(1.0, -1.0, 0.0, 1.0), (1.0, 1.0)),  // bottom-right
        (software::Vec4::new(1.0, 1.0, 0.0, 1.0), (1.0, 0.0)),   // top-right
        (software::Vec4::new(-1.0, 1.0, 0.0, 1.0), (0.0, 0.0)),  // top-left
    ];
    for (pos, (u, v)) in verts {
        push_vec4(&mut vb, pos);
        push_vec2(&mut vb, u, v);
        push_vec4(&mut vb, white);
    }

    let indices: [u16; 6] = [0, 1, 2, 0, 2, 3];

    let mut rt = software::RenderTarget::new(8, 8, software::Vec4::ZERO);
    let constants = zero_constants();
    let textures = HashMap::new();
    let sampler_states = HashMap::new();
    sm3::software::draw(
        &mut rt,
        sm3::software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: Some(&indices),
            constants: &constants,
            textures: &textures,
            sampler_states: &sampler_states,
            blend_state: state::BlendState::default(),
        },
    );

    assert_eq!(rt.get(1, 1).to_rgba8(), [255, 0, 0, 255]); // top-left
    assert_eq!(rt.get(6, 1).to_rgba8(), [0, 255, 0, 255]); // top-right
    assert_eq!(rt.get(1, 6).to_rgba8(), [0, 0, 255, 255]); // bottom-left
    assert_eq!(rt.get(6, 6).to_rgba8(), [255, 255, 255, 255]); // bottom-right

    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "6fa50059441133e99a2414be50f613190809d5373953a6e414c373be772438f7"
    );
}

#[test]
fn micro_ps3_vpos_quadrants_pixel_compare() {
    let vs = build_sm3_ir(&assemble_vs_passthrough_sm3());
    let ps = build_sm3_ir(&assemble_ps3_vpos_quadrants_sm3_decoder());

    let decl = build_vertex_decl_pos_tex_color();

    let mut vb = Vec::new();
    let white = software::Vec4::new(1.0, 1.0, 1.0, 1.0);

    let verts = [
        (software::Vec4::new(-1.0, -1.0, 0.0, 1.0), (0.0, 1.0)), // bottom-left
        (software::Vec4::new(1.0, -1.0, 0.0, 1.0), (1.0, 1.0)),  // bottom-right
        (software::Vec4::new(1.0, 1.0, 0.0, 1.0), (1.0, 0.0)),   // top-right
        (software::Vec4::new(-1.0, 1.0, 0.0, 1.0), (0.0, 0.0)),  // top-left
    ];
    for (pos, (u, v)) in verts {
        push_vec4(&mut vb, pos);
        push_vec2(&mut vb, u, v);
        push_vec4(&mut vb, white);
    }

    let indices: [u16; 6] = [0, 1, 2, 0, 2, 3];

    let mut rt = software::RenderTarget::new(8, 8, software::Vec4::ZERO);
    let constants = zero_constants();
    let textures = HashMap::new();
    let sampler_states = HashMap::new();
    sm3::software::draw(
        &mut rt,
        sm3::software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: Some(&indices),
            constants: &constants,
            textures: &textures,
            sampler_states: &sampler_states,
            blend_state: state::BlendState::default(),
        },
    );

    assert_eq!(rt.get(1, 1).to_rgba8(), [255, 0, 0, 255]); // top-left
    assert_eq!(rt.get(6, 1).to_rgba8(), [0, 255, 0, 255]); // top-right
    assert_eq!(rt.get(1, 6).to_rgba8(), [0, 0, 255, 255]); // bottom-left
    assert_eq!(rt.get(6, 6).to_rgba8(), [255, 255, 255, 255]); // bottom-right

    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "6fa50059441133e99a2414be50f613190809d5373953a6e414c373be772438f7"
    );
}

#[test]
fn micro_ps3_vface_pixel_compare() {
    let vs = build_sm3_ir(&assemble_vs_passthrough_sm3());
    let ps = build_sm3_ir(&assemble_ps3_vface_sm3_decoder());

    let decl = build_vertex_decl_pos_tex_color();

    let mut vb = Vec::new();
    let white = software::Vec4::new(1.0, 1.0, 1.0, 1.0);

    let verts = [
        (software::Vec4::new(-1.0, -1.0, 0.0, 1.0), (0.0, 1.0)), // bottom-left
        (software::Vec4::new(1.0, -1.0, 0.0, 1.0), (1.0, 1.0)),  // bottom-right
        (software::Vec4::new(1.0, 1.0, 0.0, 1.0), (1.0, 0.0)),   // top-right
        (software::Vec4::new(-1.0, 1.0, 0.0, 1.0), (0.0, 0.0)),  // top-left
    ];
    for (pos, (u, v)) in verts {
        push_vec4(&mut vb, pos);
        push_vec2(&mut vb, u, v);
        push_vec4(&mut vb, white);
    }

    // One triangle is clockwise and one is counter-clockwise so `vFace` differs across the quad.
    let indices: [u16; 6] = [0, 2, 1, 0, 2, 3];

    let mut rt = software::RenderTarget::new(8, 8, software::Vec4::ZERO);
    let constants = zero_constants();
    let textures = HashMap::new();
    let sampler_states = HashMap::new();
    sm3::software::draw(
        &mut rt,
        sm3::software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: Some(&indices),
            constants: &constants,
            textures: &textures,
            sampler_states: &sampler_states,
            blend_state: state::BlendState::default(),
        },
    );

    // Top-left is covered by the CCW triangle (back-facing => blue); bottom-right by the CW
    // triangle (front-facing => red).
    assert_eq!(rt.get(1, 1).to_rgba8(), [0, 0, 255, 255]); // top-left
    assert_eq!(rt.get(6, 6).to_rgba8(), [255, 0, 0, 255]); // bottom-right

    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "dec9601eefd47e1a63253c5b993e3c1c9919b8fdeae95f131c358f66c33dc90e"
    );
}

#[test]
fn micro_ps3_ifc_def_pixel_compare() {
    let vs = shader::to_ir(&shader::parse(&to_bytes(&assemble_vs_passthrough())).unwrap());
    let ps = shader::to_ir(&shader::parse(&to_bytes(&assemble_ps3_tex_ifc_def())).unwrap());

    let decl = build_vertex_decl_pos_tex_color();

    let mut vb = Vec::new();
    let white = software::Vec4::new(1.0, 1.0, 1.0, 1.0);

    let verts = [
        (software::Vec4::new(-1.0, -1.0, 0.0, 1.0), (0.0, 1.0)), // bottom-left
        (software::Vec4::new(1.0, -1.0, 0.0, 1.0), (1.0, 1.0)),  // bottom-right
        (software::Vec4::new(1.0, 1.0, 0.0, 1.0), (1.0, 0.0)),   // top-right
        (software::Vec4::new(-1.0, 1.0, 0.0, 1.0), (0.0, 0.0)),  // top-left
    ];
    for (pos, (u, v)) in verts {
        push_vec4(&mut vb, pos);
        push_vec2(&mut vb, u, v);
        push_vec4(&mut vb, white);
    }

    let indices: [u16; 6] = [0, 1, 2, 0, 2, 3];

    // 2x2 texture with red in the left column and black in the right column.
    let tex_bytes: [u8; 16] = [
        255, 0, 0, 255, // top-left red
        0, 0, 0, 255, // top-right black
        255, 0, 0, 255, // bottom-left red
        0, 0, 0, 255, // bottom-right black
    ];
    let tex = software::Texture2D::from_rgba8(2, 2, &tex_bytes);

    let mut textures = HashMap::new();
    textures.insert(0u16, tex.into());

    let mut sampler_states = HashMap::new();
    sampler_states.insert(
        0u16,
        state::SamplerState {
            min_filter: state::FilterMode::Point,
            mag_filter: state::FilterMode::Point,
            address_u: state::AddressMode::Clamp,
            address_v: state::AddressMode::Clamp,
        },
    );

    let mut rt = software::RenderTarget::new(8, 8, software::Vec4::ZERO);
    let constants = zero_constants();
    software::draw(
        &mut rt,
        software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: Some(&indices),
            constants: &constants,
            textures: &textures,
            sampler_states: &sampler_states,
            blend_state: state::BlendState::default(),
        },
    );

    // Left side: r0.x is 1.0 so branch returns the sampled texel (red).
    assert_eq!(rt.get(1, 4).to_rgba8(), [255, 0, 0, 255]);
    // Right side: r0.x is 0.0 so branch returns the embedded constant c0 = (0.5, 0.0, 1.0, 1.0).
    assert_eq!(rt.get(6, 4).to_rgba8(), [128, 0, 255, 255]);

    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "fa291c33b86c387331d23b7163e6622bb9553e866980db89570ac967770c0ee3"
    );
}

#[test]
fn micro_ps3_defb_if_pixel_compare() {
    let vs = shader::to_ir(&shader::parse(&to_bytes(&assemble_vs_passthrough())).unwrap());
    let decl = build_vertex_decl_pos_tex_color();

    let mut vb = Vec::new();
    let white = software::Vec4::new(1.0, 1.0, 1.0, 1.0);
    for (pos_x, pos_y) in [(-0.5, -0.5), (0.5, -0.5), (0.0, 0.5)] {
        push_vec4(&mut vb, software::Vec4::new(pos_x, pos_y, 0.0, 1.0));
        push_vec2(&mut vb, 0.0, 0.0);
        push_vec4(&mut vb, white);
    }

    let constants = zero_constants();
    let textures = HashMap::new();
    let sampler_states = HashMap::new();

    for (branch, expected, expected_wgsl) in [
        (
            true,
            [255, 0, 0, 255],
            "let b0: vec4<f32> = vec4<f32>(1.0);",
        ),
        (
            false,
            [0, 255, 0, 255],
            "let b0: vec4<f32> = vec4<f32>(0.0);",
        ),
    ] {
        let ps = shader::to_ir(&shader::parse(&to_bytes(&assemble_ps3_defb_if(branch))).unwrap());

        let wgsl = shader::generate_wgsl(&ps).unwrap();
        assert!(wgsl.wgsl.contains(expected_wgsl));

        let mut rt = software::RenderTarget::new(16, 16, software::Vec4::ZERO);
        software::draw(
            &mut rt,
            software::DrawParams {
                vs: &vs,
                ps: &ps,
                vertex_decl: &decl,
                vertex_buffer: &vb,
                indices: None,
                constants: &constants,
                textures: &textures,
                sampler_states: &sampler_states,
                blend_state: state::BlendState::default(),
            },
        );

        assert_eq!(rt.get(8, 8).to_rgba8(), expected);
    }
}

#[test]
fn micro_ps3_lrp_pixel_compare() {
    let vs = shader::to_ir(&shader::parse(&to_bytes(&assemble_vs_passthrough())).unwrap());
    let ps = shader::to_ir(&shader::parse(&to_bytes(&assemble_ps3_lrp())).unwrap());

    let decl = build_vertex_decl_pos_tex_color();

    let mut vb = Vec::new();
    let white = software::Vec4::new(1.0, 1.0, 1.0, 1.0);
    for (pos_x, pos_y) in [(-0.5, -0.5), (0.5, -0.5), (0.0, 0.5)] {
        push_vec4(&mut vb, software::Vec4::new(pos_x, pos_y, 0.0, 1.0));
        push_vec2(&mut vb, 0.0, 0.0);
        push_vec4(&mut vb, white);
    }

    let mut rt = software::RenderTarget::new(16, 16, software::Vec4::ZERO);
    let constants = zero_constants();
    let textures = HashMap::new();
    let sampler_states = HashMap::new();
    software::draw(
        &mut rt,
        software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: None,
            constants: &constants,
            textures: &textures,
            sampler_states: &sampler_states,
            blend_state: state::BlendState::default(),
        },
    );

    // Expected value:
    //   c0 = 0.25
    //   c1 = (1,0,0,1)
    //   c2 = (0,1,0,1)
    //   lrp = c0*c1 + (1-c0)*c2 = (0.25, 0.75, 0.0, 1.0)
    assert_eq!(rt.get(8, 8).to_rgba8(), [64, 191, 0, 255]);
}

#[test]
fn micro_ps2_dp2_masked_xy_pixel_compare() {
    let vs = shader::to_ir(&shader::parse(&to_bytes(&assemble_vs_passthrough())).unwrap());
    let ps = shader::to_ir(&shader::parse(&to_bytes(&assemble_ps2_dp2_masked_xy())).unwrap());

    let decl = build_vertex_decl_pos_tex_color();

    let mut vb = Vec::new();
    let white = software::Vec4::new(1.0, 1.0, 1.0, 1.0);
    for (pos_x, pos_y) in [(-0.5, -0.5), (0.5, -0.5), (0.0, 0.5)] {
        push_vec4(&mut vb, software::Vec4::new(pos_x, pos_y, 0.0, 1.0));
        push_vec2(&mut vb, 0.0, 0.0);
        push_vec4(&mut vb, white);
    }

    let mut rt = software::RenderTarget::new(16, 16, software::Vec4::ZERO);
    let constants = zero_constants();
    let textures = HashMap::new();
    let sampler_states = HashMap::new();
    software::draw(
        &mut rt,
        software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: None,
            constants: &constants,
            textures: &textures,
            sampler_states: &sampler_states,
            blend_state: state::BlendState::default(),
        },
    );

    // Expected:
    //   c0 = (0.5, 0.25, 0, 0)
    //   c1 = (0.1, 0.2, 0.3, 0.4)
    //   mov r0, c1
    //   dp2 r0.xy, c0, c0 => dot(c0.xy, c0.xy) = 0.3125 written into x/y only
    //   => r0 = (0.3125, 0.3125, 0.3, 0.4)
    assert_eq!(rt.get(8, 8).to_rgba8(), [80, 80, 77, 102]);
}

#[test]
fn sm3_texld_cube_samples_face_colors() {
    let vs = build_sm3_ir(&assemble_vs_passthrough());
    let ps = build_sm3_ir(&assemble_ps3_texld_cube());
    let decl = build_vertex_decl_pos_tex4_color();

    let cube = software::TextureCube::new([
        software::Texture2D::from_rgba8(1, 1, &[255, 0, 0, 255]), // +X red
        software::Texture2D::from_rgba8(1, 1, &[0, 255, 0, 255]), // -X green
        software::Texture2D::from_rgba8(1, 1, &[0, 0, 255, 255]), // +Y blue
        software::Texture2D::from_rgba8(1, 1, &[255, 255, 0, 255]), // -Y yellow
        software::Texture2D::from_rgba8(1, 1, &[255, 0, 255, 255]), // +Z magenta
        software::Texture2D::from_rgba8(1, 1, &[0, 255, 255, 255]), // -Z cyan
    ]);
    let mut textures = HashMap::new();
    textures.insert(0u16, cube.into());

    let mut sampler_states = HashMap::new();
    sampler_states.insert(
        0u16,
        state::SamplerState {
            min_filter: state::FilterMode::Point,
            mag_filter: state::FilterMode::Point,
            address_u: state::AddressMode::Clamp,
            address_v: state::AddressMode::Clamp,
        },
    );

    let constants = zero_constants();
    let positions = [
        software::Vec4::new(-1.0, -1.0, 0.0, 1.0),
        software::Vec4::new(3.0, -1.0, 0.0, 1.0),
        software::Vec4::new(-1.0, 3.0, 0.0, 1.0),
    ];
    let white = software::Vec4::new(1.0, 1.0, 1.0, 1.0);

    for (dir, expected) in [
        (software::Vec4::new(1.0, 0.0, 0.0, 1.0), [255, 0, 0, 255]),
        (software::Vec4::new(-1.0, 0.0, 0.0, 1.0), [0, 255, 0, 255]),
        (software::Vec4::new(0.0, 1.0, 0.0, 1.0), [0, 0, 255, 255]),
        (software::Vec4::new(0.0, -1.0, 0.0, 1.0), [255, 255, 0, 255]),
        (software::Vec4::new(0.0, 0.0, 1.0, 1.0), [255, 0, 255, 255]),
        (software::Vec4::new(0.0, 0.0, -1.0, 1.0), [0, 255, 255, 255]),
    ] {
        let mut vb = Vec::new();
        for pos in positions {
            push_vec4(&mut vb, pos);
            push_vec4(&mut vb, dir);
            push_vec4(&mut vb, white);
        }

        let mut rt = software::RenderTarget::new(8, 8, software::Vec4::ZERO);
        sm3::software::draw(
            &mut rt,
            sm3::software::DrawParams {
                vs: &vs,
                ps: &ps,
                vertex_decl: &decl,
                vertex_buffer: &vb,
                indices: None,
                constants: &constants,
                textures: &textures,
                sampler_states: &sampler_states,
                blend_state: state::BlendState::default(),
            },
        );

        assert_eq!(rt.get(4, 4).to_rgba8(), expected, "dir={dir:?}");
    }
}

#[test]
fn sm3_predicated_mov_pixel_compare() {
    let vs = build_sm3_ir(&assemble_vs_passthrough());
    let ps = build_sm3_ir(&assemble_ps3_predicated_mov());

    let decl = build_vertex_decl_pos_tex_color();

    let quad = [
        (
            software::Vec4::new(-1.0, -1.0, 0.0, 1.0),
            (0.0, 1.0),
            software::Vec4::new(1.0, 0.0, 0.0, 1.0),
        ), // bottom-left red
        (
            software::Vec4::new(1.0, -1.0, 0.0, 1.0),
            (1.0, 1.0),
            software::Vec4::new(0.0, 0.0, 0.0, 1.0),
        ), // bottom-right black
        (
            software::Vec4::new(1.0, 1.0, 0.0, 1.0),
            (1.0, 0.0),
            software::Vec4::new(0.0, 0.0, 0.0, 1.0),
        ), // top-right black
        (
            software::Vec4::new(-1.0, 1.0, 0.0, 1.0),
            (0.0, 0.0),
            software::Vec4::new(1.0, 0.0, 0.0, 1.0),
        ), // top-left red
    ];
    let indices: [u16; 6] = [0, 1, 2, 0, 2, 3];

    let mut vb = Vec::new();
    for (pos, (u, v), color) in quad {
        push_vec4(&mut vb, pos);
        push_vec2(&mut vb, u, v);
        push_vec4(&mut vb, color);
    }

    let mut rt = software::RenderTarget::new(8, 8, software::Vec4::ZERO);
    let constants = zero_constants();
    sm3::software::draw(
        &mut rt,
        sm3::software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: Some(&indices),
            constants: &constants,
            textures: &HashMap::new(),
            sampler_states: &HashMap::new(),
            blend_state: state::BlendState::default(),
        },
    );

    // Left side: v0.x = 1.0 so predicate is true, output red.
    assert_eq!(rt.get(1, 4).to_rgba8(), [255, 0, 0, 255]);
    // Right side: v0.x = 0.0 so predicate is false, output blue.
    assert_eq!(rt.get(6, 4).to_rgba8(), [0, 0, 255, 255]);

    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "96055b069d3aa23d0ac33ad4f4a7d443a8d511620cf2d63269d89e5fd0c2bf2b"
    );
}

#[test]
fn sm3_mova_relative_const_pixel_compare() {
    let vs = build_sm3_ir(&assemble_vs_passthrough());
    let ps = build_sm3_ir(&assemble_ps3_mova_relative_const());

    let decl = build_vertex_decl_pos_tex_color();

    let quad = [
        (
            software::Vec4::new(-1.0, -1.0, 0.0, 1.0),
            (0.0, 1.0),
            software::Vec4::new(1.0, 1.0, 1.0, 1.0),
        ),
        (
            software::Vec4::new(1.0, -1.0, 0.0, 1.0),
            (1.0, 1.0),
            software::Vec4::new(1.0, 1.0, 1.0, 1.0),
        ),
        (
            software::Vec4::new(1.0, 1.0, 0.0, 1.0),
            (1.0, 0.0),
            software::Vec4::new(1.0, 1.0, 1.0, 1.0),
        ),
        (
            software::Vec4::new(-1.0, 1.0, 0.0, 1.0),
            (0.0, 0.0),
            software::Vec4::new(1.0, 1.0, 1.0, 1.0),
        ),
    ];
    let indices: [u16; 6] = [0, 1, 2, 0, 2, 3];

    let mut vb = Vec::new();
    for (pos, (u, v), color) in quad {
        push_vec4(&mut vb, pos);
        push_vec2(&mut vb, u, v);
        push_vec4(&mut vb, color);
    }

    let mut rt = software::RenderTarget::new(8, 8, software::Vec4::ZERO);
    let constants = zero_constants();
    sm3::software::draw(
        &mut rt,
        sm3::software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: Some(&indices),
            constants: &constants,
            textures: &HashMap::new(),
            sampler_states: &HashMap::new(),
            blend_state: state::BlendState::default(),
        },
    );

    // Left side: t0.x < 0.5 so mova_x2 truncates to 0, output c1 (red).
    assert_eq!(rt.get(1, 4).to_rgba8(), [255, 0, 0, 255]);
    // Right side: t0.x >= 0.5 so mova_x2 truncates to 1, output c2 (blue).
    assert_eq!(rt.get(6, 4).to_rgba8(), [0, 0, 255, 255]);

    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "96055b069d3aa23d0ac33ad4f4a7d443a8d511620cf2d63269d89e5fd0c2bf2b"
    );
}

#[test]
fn sm3_mova_sat_relative_const_pixel_compare() {
    let vs = build_sm3_ir(&assemble_vs_passthrough());
    let ps = build_sm3_ir(&assemble_ps3_mova_sat_relative_const());

    let decl = build_vertex_decl_pos_tex_color();

    let mut vb = Vec::new();
    for (pos_x, pos_y) in [(-0.5, -0.5), (0.5, -0.5), (0.0, 0.5)] {
        push_vec4(&mut vb, software::Vec4::new(pos_x, pos_y, 0.0, 1.0));
        push_vec2(&mut vb, 0.0, 0.0);
        push_vec4(&mut vb, software::Vec4::new(1.0, 1.0, 1.0, 1.0));
    }

    let mut rt = software::RenderTarget::new(8, 8, software::Vec4::ZERO);
    let constants = zero_constants();
    sm3::software::draw(
        &mut rt,
        sm3::software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: None,
            constants: &constants,
            textures: &HashMap::new(),
            sampler_states: &HashMap::new(),
            blend_state: state::BlendState::default(),
        },
    );

    assert_eq!(rt.get(4, 4).to_rgba8(), [255, 0, 0, 255]);
    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "ae1a8e1ea93708f5a75c5932aee528e3fc81af1ab92ef21524a7f1fea3e8705b"
    );
}

#[test]
fn sm3_mova_sat_div2_relative_const_pixel_compare() {
    let vs = build_sm3_ir(&assemble_vs_passthrough());
    let ps = build_sm3_ir(&assemble_ps3_mova_sat_div2_relative_const());

    let decl = build_vertex_decl_pos_tex_color();

    let mut vb = Vec::new();
    for (pos_x, pos_y) in [(-0.5, -0.5), (0.5, -0.5), (0.0, 0.5)] {
        push_vec4(&mut vb, software::Vec4::new(pos_x, pos_y, 0.0, 1.0));
        push_vec2(&mut vb, 0.0, 0.0);
        push_vec4(&mut vb, software::Vec4::new(1.0, 1.0, 1.0, 1.0));
    }

    let mut rt = software::RenderTarget::new(8, 8, software::Vec4::ZERO);
    let constants = zero_constants();
    sm3::software::draw(
        &mut rt,
        sm3::software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: None,
            constants: &constants,
            textures: &HashMap::new(),
            sampler_states: &HashMap::new(),
            blend_state: state::BlendState::default(),
        },
    );

    // With shift then saturate, a0.x becomes 1 and we read c2 (magenta).
    assert_eq!(rt.get(4, 4).to_rgba8(), [255, 0, 255, 255]);
    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "af9fe8e6db038a9493f63de2b3ee0c525e4bab083a824ff1d781ce6aabcab87e"
    );
}

#[test]
fn sm3_predicated_mova_relative_const_pixel_compare() {
    let vs = build_sm3_ir(&assemble_vs_passthrough());
    let ps = build_sm3_ir(&assemble_ps3_predicated_mova_relative_const());

    let decl = build_vertex_decl_pos_tex_color();

    let mut vb = Vec::new();
    for (pos_x, pos_y) in [(-0.5, -0.5), (0.5, -0.5), (0.0, 0.5)] {
        push_vec4(&mut vb, software::Vec4::new(pos_x, pos_y, 0.0, 1.0));
        push_vec2(&mut vb, 0.0, 0.0);
        push_vec4(&mut vb, software::Vec4::new(1.0, 1.0, 1.0, 1.0));
    }

    let mut rt = software::RenderTarget::new(8, 8, software::Vec4::ZERO);
    let constants = zero_constants();
    sm3::software::draw(
        &mut rt,
        sm3::software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: None,
            constants: &constants,
            textures: &HashMap::new(),
            sampler_states: &HashMap::new(),
            blend_state: state::BlendState::default(),
        },
    );

    assert_eq!(rt.get(4, 4).to_rgba8(), [255, 0, 0, 255]);
    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "ae1a8e1ea93708f5a75c5932aee528e3fc81af1ab92ef21524a7f1fea3e8705b"
    );
}

#[test]
fn sm3_predicated_mova_relative_const_taken_pixel_compare() {
    let vs = build_sm3_ir(&assemble_vs_passthrough());
    let ps = build_sm3_ir(&assemble_ps3_predicated_mova_relative_const_taken());

    let decl = build_vertex_decl_pos_tex_color();

    let mut vb = Vec::new();
    for (pos_x, pos_y) in [(-0.5, -0.5), (0.5, -0.5), (0.0, 0.5)] {
        push_vec4(&mut vb, software::Vec4::new(pos_x, pos_y, 0.0, 1.0));
        push_vec2(&mut vb, 0.0, 0.0);
        push_vec4(&mut vb, software::Vec4::new(1.0, 1.0, 1.0, 1.0));
    }

    let mut rt = software::RenderTarget::new(8, 8, software::Vec4::ZERO);
    let constants = zero_constants();
    sm3::software::draw(
        &mut rt,
        sm3::software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: None,
            constants: &constants,
            textures: &HashMap::new(),
            sampler_states: &HashMap::new(),
            blend_state: state::BlendState::default(),
        },
    );

    assert_eq!(rt.get(4, 4).to_rgba8(), [0, 255, 0, 255]);
    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "32d50bc1260d5807d2440d1d2e09b5a297e3d1a90dd55d4b93dedc3236081f7b"
    );
}

#[test]
fn sm3_mova_write_mask_preserves_other_components_pixel_compare() {
    let vs = build_sm3_ir(&assemble_vs_passthrough());
    let ps = build_sm3_ir(&assemble_ps3_mova_write_mask_preserves_other_components());

    let decl = build_vertex_decl_pos_tex_color();

    let mut vb = Vec::new();
    let white = software::Vec4::new(1.0, 1.0, 1.0, 1.0);
    for (pos_x, pos_y) in [(-0.5, -0.5), (0.5, -0.5), (0.0, 0.5)] {
        push_vec4(&mut vb, software::Vec4::new(pos_x, pos_y, 0.0, 1.0));
        push_vec2(&mut vb, 0.0, 0.0);
        push_vec4(&mut vb, white);
    }

    let mut rt = software::RenderTarget::new(16, 16, software::Vec4::ZERO);
    let constants = zero_constants();
    sm3::software::draw(
        &mut rt,
        sm3::software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: None,
            constants: &constants,
            textures: &HashMap::new(),
            sampler_states: &HashMap::new(),
            blend_state: state::BlendState::default(),
        },
    );

    assert_eq!(rt.get(8, 8).to_rgba8(), [255, 0, 255, 255]);
    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "f0ea24543939c1b353690b0561b904ea57c604ffb63e4d0bf145020d213a3796"
    );
}

#[test]
fn sm3_mova_multi_component_relative_const_pixel_compare() {
    let vs = build_sm3_ir(&assemble_vs_passthrough());
    let ps = build_sm3_ir(&assemble_ps3_mova_multi_component_relative_const());

    let decl = build_vertex_decl_pos_tex_color();

    let mut vb = Vec::new();
    for (pos_x, pos_y) in [(-0.5, -0.5), (0.5, -0.5), (0.0, 0.5)] {
        push_vec4(&mut vb, software::Vec4::new(pos_x, pos_y, 0.0, 1.0));
        push_vec2(&mut vb, 0.0, 0.0);
        push_vec4(&mut vb, software::Vec4::new(1.0, 1.0, 1.0, 1.0));
    }

    let mut rt = software::RenderTarget::new(8, 8, software::Vec4::ZERO);
    let constants = zero_constants();
    sm3::software::draw(
        &mut rt,
        sm3::software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: None,
            constants: &constants,
            textures: &HashMap::new(),
            sampler_states: &HashMap::new(),
            blend_state: state::BlendState::default(),
        },
    );

    // a0.x = 1 reads c2 (red), a0.y = 2 reads c3 (green) => yellow.
    assert_eq!(rt.get(4, 4).to_rgba8(), [255, 255, 0, 255]);
    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "9c63cec05ddc8ff912be9a050e587ed8b44acfeea445c50ee41c4d7c90bf3cef"
    );
}

#[test]
fn sm3_mova_relative_const_component_y_pixel_compare() {
    let vs = build_sm3_ir(&assemble_vs_passthrough());
    let ps = build_sm3_ir(&assemble_ps3_mova_relative_const_component_y());

    let decl = build_vertex_decl_pos_tex_color();

    let mut vb = Vec::new();
    for (pos_x, pos_y) in [(-0.5, -0.5), (0.5, -0.5), (0.0, 0.5)] {
        push_vec4(&mut vb, software::Vec4::new(pos_x, pos_y, 0.0, 1.0));
        push_vec2(&mut vb, 0.0, 0.0);
        push_vec4(&mut vb, software::Vec4::new(1.0, 1.0, 1.0, 1.0));
    }

    let mut rt = software::RenderTarget::new(8, 8, software::Vec4::ZERO);
    let constants = zero_constants();
    sm3::software::draw(
        &mut rt,
        sm3::software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: None,
            constants: &constants,
            textures: &HashMap::new(),
            sampler_states: &HashMap::new(),
            blend_state: state::BlendState::default(),
        },
    );

    assert_eq!(rt.get(4, 4).to_rgba8(), [0, 0, 255, 255]);
    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "017fbc1f819fb63b7f0b17bc42d97f9be0e62d1a167ff97dc6d888895819afc4"
    );
}

#[test]
fn sm3_relative_const_clamps_index_low_pixel_compare() {
    let vs = build_sm3_ir(&assemble_vs_passthrough());
    let ps = build_sm3_ir(&assemble_ps3_relative_const_clamp_low());

    let decl = build_vertex_decl_pos_tex_color();

    let mut vb = Vec::new();
    for (pos_x, pos_y) in [(-0.5, -0.5), (0.5, -0.5), (0.0, 0.5)] {
        push_vec4(&mut vb, software::Vec4::new(pos_x, pos_y, 0.0, 1.0));
        push_vec2(&mut vb, 0.0, 0.0);
        push_vec4(&mut vb, software::Vec4::new(1.0, 1.0, 1.0, 1.0));
    }

    let mut rt = software::RenderTarget::new(8, 8, software::Vec4::ZERO);
    let constants = zero_constants();
    sm3::software::draw(
        &mut rt,
        sm3::software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: None,
            constants: &constants,
            textures: &HashMap::new(),
            sampler_states: &HashMap::new(),
            blend_state: state::BlendState::default(),
        },
    );

    assert_eq!(rt.get(4, 4).to_rgba8(), [255, 0, 0, 255]);
    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "ae1a8e1ea93708f5a75c5932aee528e3fc81af1ab92ef21524a7f1fea3e8705b"
    );
}

#[test]
fn sm3_relative_const_clamps_index_high_pixel_compare() {
    let vs = build_sm3_ir(&assemble_vs_passthrough());
    let ps = build_sm3_ir(&assemble_ps3_relative_const_clamp_high());

    let decl = build_vertex_decl_pos_tex_color();

    let mut vb = Vec::new();
    for (pos_x, pos_y) in [(-0.5, -0.5), (0.5, -0.5), (0.0, 0.5)] {
        push_vec4(&mut vb, software::Vec4::new(pos_x, pos_y, 0.0, 1.0));
        push_vec2(&mut vb, 0.0, 0.0);
        push_vec4(&mut vb, software::Vec4::new(1.0, 1.0, 1.0, 1.0));
    }

    let mut rt = software::RenderTarget::new(8, 8, software::Vec4::ZERO);
    let constants = zero_constants();
    sm3::software::draw(
        &mut rt,
        sm3::software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: None,
            constants: &constants,
            textures: &HashMap::new(),
            sampler_states: &HashMap::new(),
            blend_state: state::BlendState::default(),
        },
    );

    assert_eq!(rt.get(4, 4).to_rgba8(), [0, 255, 0, 255]);
    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "32d50bc1260d5807d2440d1d2e09b5a297e3d1a90dd55d4b93dedc3236081f7b"
    );
}

#[test]
fn sm3_loop_relative_const_pixel_compare() {
    let vs = build_sm3_ir(&assemble_vs_passthrough());
    let ps = build_sm3_ir(&assemble_ps3_loop_relative_const());

    let decl = build_vertex_decl_pos_tex_color();

    let mut vb = Vec::new();
    let white = software::Vec4::new(1.0, 1.0, 1.0, 1.0);
    for (pos_x, pos_y) in [(-0.5, -0.5), (0.5, -0.5), (0.0, 0.5)] {
        push_vec4(&mut vb, software::Vec4::new(pos_x, pos_y, 0.0, 1.0));
        push_vec2(&mut vb, 0.0, 0.0);
        push_vec4(&mut vb, white);
    }

    let mut rt = software::RenderTarget::new(16, 16, software::Vec4::ZERO);
    let constants = zero_constants();
    sm3::software::draw(
        &mut rt,
        sm3::software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: None,
            constants: &constants,
            textures: &HashMap::new(),
            sampler_states: &HashMap::new(),
            blend_state: state::BlendState::default(),
        },
    );

    // Two loop iterations add c0 (red) + c1 (blue) => magenta (alpha clamps to 1).
    assert_eq!(rt.get(8, 8).to_rgba8(), [255, 0, 255, 255]);
    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "f0ea24543939c1b353690b0561b904ea57c604ffb63e4d0bf145020d213a3796"
    );
}

#[test]
fn sm3_rep_relative_const_pixel_compare() {
    let vs = build_sm3_ir(&assemble_vs_passthrough());
    let ps = build_sm3_ir(&assemble_ps3_rep_relative_const());

    let decl = build_vertex_decl_pos_tex_color();

    let mut vb = Vec::new();
    let white = software::Vec4::new(1.0, 1.0, 1.0, 1.0);
    for (pos_x, pos_y) in [(-0.5, -0.5), (0.5, -0.5), (0.0, 0.5)] {
        push_vec4(&mut vb, software::Vec4::new(pos_x, pos_y, 0.0, 1.0));
        push_vec2(&mut vb, 0.0, 0.0);
        push_vec4(&mut vb, white);
    }

    let mut rt = software::RenderTarget::new(16, 16, software::Vec4::ZERO);
    let constants = zero_constants();
    sm3::software::draw(
        &mut rt,
        sm3::software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: None,
            constants: &constants,
            textures: &HashMap::new(),
            sampler_states: &HashMap::new(),
            blend_state: state::BlendState::default(),
        },
    );

    // Two rep iterations add c0 (red) + c1 (blue) => magenta (alpha clamps to 1).
    assert_eq!(rt.get(8, 8).to_rgba8(), [255, 0, 255, 255]);
    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "f0ea24543939c1b353690b0561b904ea57c604ffb63e4d0bf145020d213a3796"
    );
}

#[test]
fn sm3_rep_relative_const_high_int_index_pixel_compare() {
    let vs = build_sm3_ir(&assemble_vs_passthrough());
    let ps = build_sm3_ir(&assemble_ps3_rep_relative_const_high_int_index());

    let decl = build_vertex_decl_pos_tex_color();

    let mut vb = Vec::new();
    let white = software::Vec4::new(1.0, 1.0, 1.0, 1.0);
    for (pos_x, pos_y) in [(-0.5, -0.5), (0.5, -0.5), (0.0, 0.5)] {
        push_vec4(&mut vb, software::Vec4::new(pos_x, pos_y, 0.0, 1.0));
        push_vec2(&mut vb, 0.0, 0.0);
        push_vec4(&mut vb, white);
    }

    let mut rt = software::RenderTarget::new(16, 16, software::Vec4::ZERO);
    let constants = zero_constants();
    sm3::software::draw(
        &mut rt,
        sm3::software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: None,
            constants: &constants,
            textures: &HashMap::new(),
            sampler_states: &HashMap::new(),
            blend_state: state::BlendState::default(),
        },
    );

    // Two rep iterations add c0 (red) + c1 (blue) => magenta (alpha clamps to 1).
    assert_eq!(rt.get(8, 8).to_rgba8(), [255, 0, 255, 255]);
    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "f0ea24543939c1b353690b0561b904ea57c604ffb63e4d0bf145020d213a3796"
    );
}

#[test]
fn sm3_defb_high_index_pixel_compare() {
    let vs = build_sm3_ir(&assemble_vs_passthrough());
    let ps = build_sm3_ir(&assemble_ps3_defb_if_high_index(true));

    let decl = build_vertex_decl_pos_tex_color();

    let mut vb = Vec::new();
    let white = software::Vec4::new(1.0, 1.0, 1.0, 1.0);
    for (pos_x, pos_y) in [(-0.5, -0.5), (0.5, -0.5), (0.0, 0.5)] {
        push_vec4(&mut vb, software::Vec4::new(pos_x, pos_y, 0.0, 1.0));
        push_vec2(&mut vb, 0.0, 0.0);
        push_vec4(&mut vb, white);
    }

    let mut rt = software::RenderTarget::new(16, 16, software::Vec4::ZERO);
    let constants = zero_constants();
    sm3::software::draw(
        &mut rt,
        sm3::software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: None,
            constants: &constants,
            textures: &HashMap::new(),
            sampler_states: &HashMap::new(),
            blend_state: state::BlendState::default(),
        },
    );

    // `defb b20, true` should take the if-branch and output red.
    assert_eq!(rt.get(8, 8).to_rgba8(), [255, 0, 0, 255]);
    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "f319f67af7e26fb3e108840dfe953de674f251a9542b12738334ad592fbff483"
    );
}

#[test]
fn sm3_exp_log_pow_pixel_compare() {
    let vs = build_sm3_ir(&assemble_vs_passthrough_sm3());
    let ps = build_sm3_ir(&assemble_ps3_exp_log_pow());

    let decl = build_vertex_decl_pos_tex_color();

    let quad = [
        (
            software::Vec4::new(-1.0, -1.0, 0.0, 1.0),
            (0.0, 1.0),
            software::Vec4::new(1.0, 1.0, 1.0, 1.0),
        ),
        (
            software::Vec4::new(1.0, -1.0, 0.0, 1.0),
            (1.0, 1.0),
            software::Vec4::new(1.0, 1.0, 1.0, 1.0),
        ),
        (
            software::Vec4::new(1.0, 1.0, 0.0, 1.0),
            (1.0, 0.0),
            software::Vec4::new(1.0, 1.0, 1.0, 1.0),
        ),
        (
            software::Vec4::new(-1.0, 1.0, 0.0, 1.0),
            (0.0, 0.0),
            software::Vec4::new(1.0, 1.0, 1.0, 1.0),
        ),
    ];
    let indices: [u16; 6] = [0, 1, 2, 0, 2, 3];

    let mut vb = Vec::new();
    for (pos, (u, v), color) in quad {
        push_vec4(&mut vb, pos);
        push_vec2(&mut vb, u, v);
        push_vec4(&mut vb, color);
    }

    let mut rt = software::RenderTarget::new(8, 8, software::Vec4::ZERO);
    let constants = zero_constants();
    sm3::software::draw(
        &mut rt,
        sm3::software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: Some(&indices),
            constants: &constants,
            textures: &HashMap::new(),
            sampler_states: &HashMap::new(),
            blend_state: state::BlendState::default(),
        },
    );

    // R = exp2(-2.0) = 0.25, G = log2(2.0) = 1.0, B = pow(0.25, 2.0) = 0.0625, A = 1.0.
    assert_eq!(rt.get(4, 4).to_rgba8(), [64, 255, 16, 255]);

    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "1806680cf63f0d89928fe033c641adc922232f74f257867de050efb43f50edb9"
    );
}

#[test]
fn sm3_exp_log_pow_sat_x2_order_pixel_compare() {
    let vs = build_sm3_ir(&assemble_vs_passthrough_sm3());
    let ps = build_sm3_ir(&assemble_ps3_exp_log_pow_sat_x2_order());

    let decl = build_vertex_decl_pos_tex_color();

    let quad = [
        (
            software::Vec4::new(-1.0, -1.0, 0.0, 1.0),
            (0.0, 1.0),
            software::Vec4::new(1.0, 1.0, 1.0, 1.0),
        ),
        (
            software::Vec4::new(1.0, -1.0, 0.0, 1.0),
            (1.0, 1.0),
            software::Vec4::new(1.0, 1.0, 1.0, 1.0),
        ),
        (
            software::Vec4::new(1.0, 1.0, 0.0, 1.0),
            (1.0, 0.0),
            software::Vec4::new(1.0, 1.0, 1.0, 1.0),
        ),
        (
            software::Vec4::new(-1.0, 1.0, 0.0, 1.0),
            (0.0, 0.0),
            software::Vec4::new(1.0, 1.0, 1.0, 1.0),
        ),
    ];
    let indices: [u16; 6] = [0, 1, 2, 0, 2, 3];

    let mut vb = Vec::new();
    for (pos, (u, v), color) in quad {
        push_vec4(&mut vb, pos);
        push_vec2(&mut vb, u, v);
        push_vec4(&mut vb, color);
    }

    let mut rt = software::RenderTarget::new(8, 8, software::Vec4::ZERO);
    let constants = zero_constants();
    sm3::software::draw(
        &mut rt,
        sm3::software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: Some(&indices),
            constants: &constants,
            textures: &HashMap::new(),
            sampler_states: &HashMap::new(),
            blend_state: state::BlendState::default(),
        },
    );

    // Each math op uses `sat_x2` but is then multiplied by 0.25; this keeps the final value in
    // range while still validating the result-modifier order (shift before saturate).
    assert_eq!(rt.get(4, 4).to_rgba8(), [64, 64, 64, 255]);

    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "6faf128775a825392b4e9f890b11c8c9000d945aefdb18b2f244f3af397fd8a1"
    );
}

#[test]
fn sm3_exp_componentwise_pixel_compare() {
    let vs = build_sm3_ir(&assemble_vs_passthrough());
    let ps = build_sm3_ir(&assemble_ps3_exp_components());

    let decl = build_vertex_decl_pos_tex_color();

    let quad = [
        (
            software::Vec4::new(-1.0, -1.0, 0.0, 1.0),
            (0.0, 1.0),
            software::Vec4::new(1.0, 1.0, 1.0, 1.0),
        ),
        (
            software::Vec4::new(1.0, -1.0, 0.0, 1.0),
            (1.0, 1.0),
            software::Vec4::new(1.0, 1.0, 1.0, 1.0),
        ),
        (
            software::Vec4::new(1.0, 1.0, 0.0, 1.0),
            (1.0, 0.0),
            software::Vec4::new(1.0, 1.0, 1.0, 1.0),
        ),
        (
            software::Vec4::new(-1.0, 1.0, 0.0, 1.0),
            (0.0, 0.0),
            software::Vec4::new(1.0, 1.0, 1.0, 1.0),
        ),
    ];
    let indices: [u16; 6] = [0, 1, 2, 0, 2, 3];

    let mut vb = Vec::new();
    for (pos, (u, v), color) in quad {
        push_vec4(&mut vb, pos);
        push_vec2(&mut vb, u, v);
        push_vec4(&mut vb, color);
    }

    let mut rt = software::RenderTarget::new(8, 8, software::Vec4::ZERO);
    let constants = zero_constants();
    sm3::software::draw(
        &mut rt,
        sm3::software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: Some(&indices),
            constants: &constants,
            textures: &HashMap::new(),
            sampler_states: &HashMap::new(),
            blend_state: state::BlendState::default(),
        },
    );

    // exp2([-2, -1, 0, -3]) = [0.25, 0.5, 1.0, 0.125]
    assert_eq!(rt.get(4, 4).to_rgba8(), [64, 128, 255, 32]);

    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "c6627184f5c5688408f0fc672a1a28d97e032d3f8ec538a5b37a26ce1a03b7d1"
    );
}

#[test]
fn sm3_log_componentwise_div8_pixel_compare() {
    let vs = build_sm3_ir(&assemble_vs_passthrough());
    let ps = build_sm3_ir(&assemble_ps3_log_components_div8());

    let decl = build_vertex_decl_pos_tex_color();

    let quad = [
        (
            software::Vec4::new(-1.0, -1.0, 0.0, 1.0),
            (0.0, 1.0),
            software::Vec4::new(1.0, 1.0, 1.0, 1.0),
        ),
        (
            software::Vec4::new(1.0, -1.0, 0.0, 1.0),
            (1.0, 1.0),
            software::Vec4::new(1.0, 1.0, 1.0, 1.0),
        ),
        (
            software::Vec4::new(1.0, 1.0, 0.0, 1.0),
            (1.0, 0.0),
            software::Vec4::new(1.0, 1.0, 1.0, 1.0),
        ),
        (
            software::Vec4::new(-1.0, 1.0, 0.0, 1.0),
            (0.0, 0.0),
            software::Vec4::new(1.0, 1.0, 1.0, 1.0),
        ),
    ];
    let indices: [u16; 6] = [0, 1, 2, 0, 2, 3];

    let mut vb = Vec::new();
    for (pos, (u, v), color) in quad {
        push_vec4(&mut vb, pos);
        push_vec2(&mut vb, u, v);
        push_vec4(&mut vb, color);
    }

    let mut rt = software::RenderTarget::new(8, 8, software::Vec4::ZERO);
    let constants = zero_constants();
    sm3::software::draw(
        &mut rt,
        sm3::software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: Some(&indices),
            constants: &constants,
            textures: &HashMap::new(),
            sampler_states: &HashMap::new(),
            blend_state: state::BlendState::default(),
        },
    );

    // log2([1, 2, 4, 8]) / 8 = [0.0, 0.125, 0.25, 0.375]
    assert_eq!(rt.get(4, 4).to_rgba8(), [0, 32, 64, 96]);

    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "9baabd14a2a68587a46f826d461c7c9945a4e70636bf0c807fd1e5b825893634"
    );
}

#[test]
fn sm3_pow_componentwise_pixel_compare() {
    let vs = build_sm3_ir(&assemble_vs_passthrough());
    let ps = build_sm3_ir(&assemble_ps3_pow_components());

    let decl = build_vertex_decl_pos_tex_color();

    let quad = [
        (
            software::Vec4::new(-1.0, -1.0, 0.0, 1.0),
            (0.0, 1.0),
            software::Vec4::new(1.0, 1.0, 1.0, 1.0),
        ),
        (
            software::Vec4::new(1.0, -1.0, 0.0, 1.0),
            (1.0, 1.0),
            software::Vec4::new(1.0, 1.0, 1.0, 1.0),
        ),
        (
            software::Vec4::new(1.0, 1.0, 0.0, 1.0),
            (1.0, 0.0),
            software::Vec4::new(1.0, 1.0, 1.0, 1.0),
        ),
        (
            software::Vec4::new(-1.0, 1.0, 0.0, 1.0),
            (0.0, 0.0),
            software::Vec4::new(1.0, 1.0, 1.0, 1.0),
        ),
    ];
    let indices: [u16; 6] = [0, 1, 2, 0, 2, 3];

    let mut vb = Vec::new();
    for (pos, (u, v), color) in quad {
        push_vec4(&mut vb, pos);
        push_vec2(&mut vb, u, v);
        push_vec4(&mut vb, color);
    }

    let mut rt = software::RenderTarget::new(8, 8, software::Vec4::ZERO);
    let constants = zero_constants();
    sm3::software::draw(
        &mut rt,
        sm3::software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: Some(&indices),
            constants: &constants,
            textures: &HashMap::new(),
            sampler_states: &HashMap::new(),
            blend_state: state::BlendState::default(),
        },
    );

    // pow([0.25, 0.5, 0.75, 1.0], [2.0, 2.0, 2.0, 0.0]) = [0.0625, 0.25, 0.5625, 1.0]
    assert_eq!(rt.get(4, 4).to_rgba8(), [16, 64, 143, 255]);

    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "d4e33a0c9698fa3b3768658b91573873dd99119507d8b1638e0ceaab4f07135b"
    );
}

#[test]
fn sm3_dp2_constant_pixel_compare() {
    let vs = build_sm3_ir(&assemble_vs_passthrough());
    let ps = build_sm3_ir(&assemble_ps3_dp2_constant());

    let decl = build_vertex_decl_pos_tex_color();

    let quad = [
        (
            software::Vec4::new(-1.0, -1.0, 0.0, 1.0),
            (0.0, 1.0),
            software::Vec4::new(1.0, 1.0, 1.0, 1.0),
        ),
        (
            software::Vec4::new(1.0, -1.0, 0.0, 1.0),
            (1.0, 1.0),
            software::Vec4::new(1.0, 1.0, 1.0, 1.0),
        ),
        (
            software::Vec4::new(1.0, 1.0, 0.0, 1.0),
            (1.0, 0.0),
            software::Vec4::new(1.0, 1.0, 1.0, 1.0),
        ),
        (
            software::Vec4::new(-1.0, 1.0, 0.0, 1.0),
            (0.0, 0.0),
            software::Vec4::new(1.0, 1.0, 1.0, 1.0),
        ),
    ];
    let indices: [u16; 6] = [0, 1, 2, 0, 2, 3];

    let mut vb = Vec::new();
    for (pos, (u, v), color) in quad {
        push_vec4(&mut vb, pos);
        push_vec2(&mut vb, u, v);
        push_vec4(&mut vb, color);
    }

    let mut rt = software::RenderTarget::new(8, 8, software::Vec4::ZERO);
    let constants = zero_constants();
    sm3::software::draw(
        &mut rt,
        sm3::software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: Some(&indices),
            constants: &constants,
            textures: &HashMap::new(),
            sampler_states: &HashMap::new(),
            blend_state: state::BlendState::default(),
        },
    );

    // dp2 result = dot(c0.zw, c1.yx) = 0.25 * 1.0 + 0.5 * 0.5 = 0.5.
    assert_eq!(rt.get(4, 4).to_rgba8(), [128, 128, 128, 255]);

    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "23ac9a3eadbe0b53bf8c503a2ea1d36b41d487bfaf72abc450387b3b6ae9bfa5"
    );
}

#[test]
fn sm3_predicated_exp_log_pow_with_modifiers_pixel_compare() {
    let vs = build_sm3_ir(&assemble_vs_passthrough());
    let ps = build_sm3_ir(&assemble_ps3_predicated_exp_log_pow_modifiers());

    let decl = build_vertex_decl_pos_tex_color();

    let quad = [
        // Left side is red (v0.x=1.0) so predicate is true.
        (
            software::Vec4::new(-1.0, -1.0, 0.0, 1.0),
            (0.0, 1.0),
            software::Vec4::new(1.0, 0.0, 0.0, 1.0),
        ),
        // Right side is black (v0.x=0.0) so predicate is false.
        (
            software::Vec4::new(1.0, -1.0, 0.0, 1.0),
            (1.0, 1.0),
            software::Vec4::new(0.0, 0.0, 0.0, 1.0),
        ),
        (
            software::Vec4::new(1.0, 1.0, 0.0, 1.0),
            (1.0, 0.0),
            software::Vec4::new(0.0, 0.0, 0.0, 1.0),
        ),
        (
            software::Vec4::new(-1.0, 1.0, 0.0, 1.0),
            (0.0, 0.0),
            software::Vec4::new(1.0, 0.0, 0.0, 1.0),
        ),
    ];
    let indices: [u16; 6] = [0, 1, 2, 0, 2, 3];

    let mut vb = Vec::new();
    for (pos, (u, v), color) in quad {
        push_vec4(&mut vb, pos);
        push_vec2(&mut vb, u, v);
        push_vec4(&mut vb, color);
    }

    let mut rt = software::RenderTarget::new(8, 8, software::Vec4::ZERO);
    let constants = zero_constants();
    sm3::software::draw(
        &mut rt,
        sm3::software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: Some(&indices),
            constants: &constants,
            textures: &HashMap::new(),
            sampler_states: &HashMap::new(),
            blend_state: state::BlendState::default(),
        },
    );

    // Left side: predicate true. `sat_x2` result modifiers are applied to each op:
    // - exp2(-2)*2 = 0.5
    // - log2(1.25)*2 ~= 0.643856
    // - pow(0.25, 2)*2 = 0.125
    assert_eq!(rt.get(1, 4).to_rgba8(), [128, 164, 32, 255]);
    // Right side: predicate false, output stays at the default blue constant.
    assert_eq!(rt.get(6, 4).to_rgba8(), [0, 0, 255, 255]);

    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "ce434184a3c5460d276eb05eb0e4561574b5687d80b85b587158f774dd65091e"
    );
}

#[test]
fn sm3_nrm_pixel_compare() {
    let vs = build_sm3_ir(&assemble_vs_passthrough());
    let ps = build_sm3_ir(&assemble_ps3_nrm_sm3_decoder());

    let decl = build_vertex_decl_pos_tex_color();

    let quad = [
        (software::Vec4::new(-1.0, -1.0, 0.0, 1.0), (0.0, 1.0)),
        (software::Vec4::new(1.0, -1.0, 0.0, 1.0), (1.0, 1.0)),
        (software::Vec4::new(1.0, 1.0, 0.0, 1.0), (1.0, 0.0)),
        (software::Vec4::new(-1.0, 1.0, 0.0, 1.0), (0.0, 0.0)),
    ];
    let indices: [u16; 6] = [0, 1, 2, 0, 2, 3];

    let mut vb = Vec::new();
    let white = software::Vec4::new(1.0, 1.0, 1.0, 1.0);
    for (pos, (u, v)) in quad {
        push_vec4(&mut vb, pos);
        push_vec2(&mut vb, u, v);
        push_vec4(&mut vb, white);
    }

    let mut rt = software::RenderTarget::new(8, 8, software::Vec4::ZERO);
    let constants = zero_constants();
    sm3::software::draw(
        &mut rt,
        sm3::software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: Some(&indices),
            constants: &constants,
            textures: &HashMap::new(),
            sampler_states: &HashMap::new(),
            blend_state: state::BlendState::default(),
        },
    );

    // normalize(vec3(3, 4, 0)) = (0.6, 0.8, 0.0), alpha=1.0
    assert_eq!(rt.get(4, 4).to_rgba8(), [153, 204, 0, 255]);

    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "1340a1f6459e5c42c08ac8ac55c2f41d26b33ee195991fc28677d853437b62a9"
    );
}

#[test]
fn sm3_lit_pixel_compare() {
    let vs = build_sm3_ir(&assemble_vs_passthrough());
    let ps = build_sm3_ir(&assemble_ps3_lit_sm3_decoder());

    let decl = build_vertex_decl_pos_tex_color();

    let quad = [
        (software::Vec4::new(-1.0, -1.0, 0.0, 1.0), (0.0, 1.0)),
        (software::Vec4::new(1.0, -1.0, 0.0, 1.0), (1.0, 1.0)),
        (software::Vec4::new(1.0, 1.0, 0.0, 1.0), (1.0, 0.0)),
        (software::Vec4::new(-1.0, 1.0, 0.0, 1.0), (0.0, 0.0)),
    ];
    let indices: [u16; 6] = [0, 1, 2, 0, 2, 3];

    let mut vb = Vec::new();
    let white = software::Vec4::new(1.0, 1.0, 1.0, 1.0);
    for (pos, (u, v)) in quad {
        push_vec4(&mut vb, pos);
        push_vec2(&mut vb, u, v);
        push_vec4(&mut vb, white);
    }

    let mut rt = software::RenderTarget::new(8, 8, software::Vec4::ZERO);
    let constants = zero_constants();
    sm3::software::draw(
        &mut rt,
        sm3::software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: Some(&indices),
            constants: &constants,
            textures: &HashMap::new(),
            sampler_states: &HashMap::new(),
            blend_state: state::BlendState::default(),
        },
    );

    // lit(0.5, 0.5, _, 2.0) = (1.0, 0.5, pow(0.5, 2.0)=0.25, 1.0)
    assert_eq!(rt.get(4, 4).to_rgba8(), [255, 128, 64, 255]);

    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "be1d6094f2901029ca3e780770b605b503320b770ef790a80c6dd352e00b1ea4"
    );
}

#[test]
fn sm3_sincos_pixel_compare() {
    let vs = build_sm3_ir(&assemble_vs_passthrough());
    let ps = build_sm3_ir(&assemble_ps3_sincos_sm3_decoder());

    let decl = build_vertex_decl_pos_tex_color();

    let quad = [
        (software::Vec4::new(-1.0, -1.0, 0.0, 1.0), (0.0, 1.0)),
        (software::Vec4::new(1.0, -1.0, 0.0, 1.0), (1.0, 1.0)),
        (software::Vec4::new(1.0, 1.0, 0.0, 1.0), (1.0, 0.0)),
        (software::Vec4::new(-1.0, 1.0, 0.0, 1.0), (0.0, 0.0)),
    ];
    let indices: [u16; 6] = [0, 1, 2, 0, 2, 3];

    let mut vb = Vec::new();
    let white = software::Vec4::new(1.0, 1.0, 1.0, 1.0);
    for (pos, (u, v)) in quad {
        push_vec4(&mut vb, pos);
        push_vec2(&mut vb, u, v);
        push_vec4(&mut vb, white);
    }

    let mut rt = software::RenderTarget::new(8, 8, software::Vec4::ZERO);
    let constants = zero_constants();
    sm3::software::draw(
        &mut rt,
        sm3::software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: Some(&indices),
            constants: &constants,
            textures: &HashMap::new(),
            sampler_states: &HashMap::new(),
            blend_state: state::BlendState::default(),
        },
    );

    // angle = c0.x * c1.x + c2.x = 1.0*2.0 + 0.5 = 2.5
    // sin(2.5) ~= 0.598, cos(2.5) ~= -0.801 -> saturate clamps cos to 0.
    assert_eq!(rt.get(4, 4).to_rgba8(), [153, 0, 0, 255]);

    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "0790007b251eedb069fd94f15e2cb2ad3bede332ddec8dd49daa06b190f7c1ec"
    );
}

#[test]
fn sm3_bounded_loop_accumulate_pixel_compare() {
    let vs = build_sm3_ir(&assemble_vs_passthrough());
    let ps = build_sm3_ir(&assemble_ps3_loop_accumulate());

    let decl = build_vertex_decl_pos_tex_color();

    let quad = [
        (
            software::Vec4::new(-1.0, -1.0, 0.0, 1.0),
            (0.0, 1.0),
            software::Vec4::new(1.0, 1.0, 1.0, 1.0),
        ),
        (
            software::Vec4::new(1.0, -1.0, 0.0, 1.0),
            (1.0, 1.0),
            software::Vec4::new(1.0, 1.0, 1.0, 1.0),
        ),
        (
            software::Vec4::new(1.0, 1.0, 0.0, 1.0),
            (1.0, 0.0),
            software::Vec4::new(1.0, 1.0, 1.0, 1.0),
        ),
        (
            software::Vec4::new(-1.0, 1.0, 0.0, 1.0),
            (0.0, 0.0),
            software::Vec4::new(1.0, 1.0, 1.0, 1.0),
        ),
    ];
    let indices: [u16; 6] = [0, 1, 2, 0, 2, 3];

    let mut vb = Vec::new();
    for (pos, (u, v), color) in quad {
        push_vec4(&mut vb, pos);
        push_vec2(&mut vb, u, v);
        push_vec4(&mut vb, color);
    }

    let mut rt = software::RenderTarget::new(8, 8, software::Vec4::ZERO);
    let constants = zero_constants();
    sm3::software::draw(
        &mut rt,
        sm3::software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: Some(&indices),
            constants: &constants,
            textures: &HashMap::new(),
            sampler_states: &HashMap::new(),
            blend_state: state::BlendState::default(),
        },
    );

    // Loop accumulates c1 four times and keeps alpha at 1.0.
    assert_eq!(rt.get(4, 4).to_rgba8(), [102, 204, 255, 255]);

    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "ecbe30cb268c55a1e0a425e36314cfc0759f659cb777deee88ab0250e8ebb275"
    );
}

#[test]
fn sm3_vertex_input_semantic_locations_pixel_compare() {
    let vs = build_sm3_ir(&assemble_vs_passthrough_with_dcl_sm3_decoder());
    assert!(vs.uses_semantic_locations);
    let ps = build_sm3_ir(&assemble_ps2_mov_oc0_t0_sm3_decoder());

    let decl = build_vertex_decl_pos_tex_color();

    let quad = [
        (software::Vec4::new(-1.0, -1.0, 0.0, 1.0), (0.0, 1.0)),
        (software::Vec4::new(1.0, -1.0, 0.0, 1.0), (1.0, 1.0)),
        (software::Vec4::new(1.0, 1.0, 0.0, 1.0), (1.0, 0.0)),
        (software::Vec4::new(-1.0, 1.0, 0.0, 1.0), (0.0, 0.0)),
    ];
    let indices: [u16; 6] = [0, 1, 2, 0, 2, 3];

    let mut vb = Vec::new();
    let white = software::Vec4::new(1.0, 1.0, 1.0, 1.0);
    for (pos, (u, v)) in quad {
        push_vec4(&mut vb, pos);
        push_vec2(&mut vb, u, v);
        push_vec4(&mut vb, white);
    }

    let mut rt = software::RenderTarget::new(8, 8, software::Vec4::ZERO);
    let constants = zero_constants();
    sm3::software::draw(
        &mut rt,
        sm3::software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: Some(&indices),
            constants: &constants,
            textures: &HashMap::new(),
            sampler_states: &HashMap::new(),
            blend_state: state::BlendState::default(),
        },
    );

    // At the center of the quad, interpolated (u, v) should be stable and non-zero.
    assert_eq!(rt.get(4, 4).to_rgba8(), [143, 143, 0, 255]);

    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "524d04e1337e7293fa80fba71bc6566addbfc46aad2f6c63656e0df4fdee75e9"
    );
}

#[test]
fn sm3_vs3_output_semantic_locations_pixel_compare() {
    let vs = build_sm3_ir(&assemble_vs3_generic_output_texcoord3_constant_sm3_decoder());
    let ps = build_sm3_ir(&assemble_ps3_input_texcoord3_passthrough_sm3_decoder());

    let decl = build_vertex_decl_pos_tex_color();

    let quad = [
        (software::Vec4::new(-1.0, -1.0, 0.0, 1.0), (0.0, 1.0)),
        (software::Vec4::new(1.0, -1.0, 0.0, 1.0), (1.0, 1.0)),
        (software::Vec4::new(1.0, 1.0, 0.0, 1.0), (1.0, 0.0)),
        (software::Vec4::new(-1.0, 1.0, 0.0, 1.0), (0.0, 0.0)),
    ];
    let indices: [u16; 6] = [0, 1, 2, 0, 2, 3];

    let mut vb = Vec::new();
    let white = software::Vec4::new(1.0, 1.0, 1.0, 1.0);
    for (pos, (u, v)) in quad {
        push_vec4(&mut vb, pos);
        push_vec2(&mut vb, u, v);
        push_vec4(&mut vb, white);
    }

    let mut rt = software::RenderTarget::new(8, 8, software::Vec4::ZERO);
    let constants = zero_constants();
    sm3::software::draw(
        &mut rt,
        sm3::software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: Some(&indices),
            constants: &constants,
            textures: &HashMap::new(),
            sampler_states: &HashMap::new(),
            blend_state: state::BlendState::default(),
        },
    );

    // TEXCOORD3 is declared on VS `o0` and PS `v0`; location matching must use the semantic
    // index (3), not the raw register index (0).
    assert_eq!(rt.get(4, 4).to_rgba8(), [64, 128, 0, 255]);

    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "613f76fe73088defc52f7cd1acad3b8d0fce920bee67651655c37dcc6d97e304"
    );
}

#[test]
fn micro_alpha_blending_pixel_compare() {
    let vs = shader::to_ir(&shader::parse(&to_bytes(&assemble_vs_passthrough())).unwrap());
    let ps = shader::to_ir(&shader::parse(&to_bytes(&assemble_ps_color_passthrough())).unwrap());

    let decl = build_vertex_decl_pos_tex_color();

    let quad = [
        (software::Vec4::new(-1.0, -1.0, 0.0, 1.0), (0.0, 1.0)),
        (software::Vec4::new(1.0, -1.0, 0.0, 1.0), (1.0, 1.0)),
        (software::Vec4::new(1.0, 1.0, 0.0, 1.0), (1.0, 0.0)),
        (software::Vec4::new(-1.0, 1.0, 0.0, 1.0), (0.0, 0.0)),
    ];
    let indices: [u16; 6] = [0, 1, 2, 0, 2, 3];

    let make_vb = |color: software::Vec4| {
        let mut vb = Vec::new();
        for (pos, (u, v)) in quad {
            push_vec4(&mut vb, pos);
            push_vec2(&mut vb, u, v);
            push_vec4(&mut vb, color);
        }
        vb
    };

    let vb_red = make_vb(software::Vec4::new(1.0, 0.0, 0.0, 1.0));
    let vb_green = make_vb(software::Vec4::new(0.0, 1.0, 0.0, 0.5));
    let textures = HashMap::new();
    let sampler_states = HashMap::new();

    let mut rt = software::RenderTarget::new(8, 8, software::Vec4::ZERO);
    let constants = zero_constants();
    // Pass 1: opaque red.
    software::draw(
        &mut rt,
        software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb_red,
            indices: Some(&indices),
            constants: &constants,
            textures: &textures,
            sampler_states: &sampler_states,
            blend_state: state::BlendState::default(),
        },
    );

    // Pass 2: green with alpha=0.5 blended over.
    let blend = state::BlendState {
        enabled: true,
        src_factor: state::BlendFactor::SrcAlpha,
        dst_factor: state::BlendFactor::OneMinusSrcAlpha,
        op: state::BlendOp::Add,
    };
    software::draw(
        &mut rt,
        software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb_green,
            indices: Some(&indices),
            constants: &constants,
            textures: &textures,
            sampler_states: &sampler_states,
            blend_state: blend,
        },
    );

    assert_eq!(rt.get(4, 4).to_rgba8(), [128, 128, 0, 191]);
    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "22e5d8454f12677044ceb24de7c5da02e285d7a6b347c7ed4bfb7b2209dadb0a"
    );
}

#[test]
fn translates_src_and_result_modifiers_to_wgsl() {
    let ps_bytes = to_bytes(&assemble_ps_mov_sat_neg_c0());
    let program = shader::parse(&ps_bytes).unwrap();
    let ir = shader::to_ir(&program);
    let wgsl = shader::generate_wgsl(&ir).unwrap();

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(wgsl.wgsl.contains("clamp("));
    // Pixel shader constants are packed after the vertex constant register file.
    assert!(wgsl.wgsl.contains("constants.c[256u]"));
}

#[test]
fn micro_ps2_src_and_result_modifiers_pixel_compare() {
    let vs = shader::to_ir(&shader::parse(&to_bytes(&assemble_vs_passthrough())).unwrap());
    let ps = shader::to_ir(&shader::parse(&to_bytes(&assemble_ps_mov_sat_neg_c0())).unwrap());

    let decl = build_vertex_decl_pos_tex_color();

    let mut vb = Vec::new();
    let white = software::Vec4::new(1.0, 1.0, 1.0, 1.0);

    for (pos_x, pos_y) in [(-0.5, -0.5), (0.5, -0.5), (0.0, 0.5)] {
        push_vec4(&mut vb, software::Vec4::new(pos_x, pos_y, 0.0, 1.0));
        push_vec2(&mut vb, 0.0, 0.0);
        push_vec4(&mut vb, white);
    }

    let mut constants = zero_constants();
    constants[0] = software::Vec4::new(-0.5, 0.5, -2.0, -1.0);

    let mut rt = software::RenderTarget::new(16, 16, software::Vec4::ZERO);
    software::draw(
        &mut rt,
        software::DrawParams {
            vs: &vs,
            ps: &ps,
            vertex_decl: &decl,
            vertex_buffer: &vb,
            indices: None,
            constants: &constants,
            textures: &HashMap::new(),
            sampler_states: &HashMap::new(),
            blend_state: state::BlendState::default(),
        },
    );

    // `oC0 = clamp(-c0, 0..1)`, with c0 = (-0.5, 0.5, -2.0, -1.0).
    assert_eq!(rt.get(8, 8).to_rgba8(), [128, 0, 255, 255]);

    let hash = blake3::hash(&rt.to_rgba8());
    assert_eq!(
        hash.to_hex().as_str(),
        "ab477a03b69b374481c3b6cba362a9b6e9cfb0dd038252a06a610b4c058e3f26"
    );
}

#[test]
fn sm3_translates_additional_src_modifiers_to_wgsl() {
    let words = assemble_ps2_src_modifiers_bias_x2neg_dz();

    let decoded = crate::sm3::decode_u32_tokens(&words).expect("decode");
    let ir = crate::sm3::build_ir(&decoded).expect("build_ir");
    crate::sm3::verify_ir(&ir).expect("verify_ir");
    let wgsl = crate::sm3::generate_wgsl(&ir).expect("generate_wgsl");

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    // Spot-check that the modifiers were lowered (not ignored).
    assert!(wgsl.wgsl.contains("vec4<f32>(0.5)"), "{}", wgsl.wgsl);
    assert!(wgsl.wgsl.contains("* 2.0"), "{}", wgsl.wgsl);
    assert!(wgsl.wgsl.contains(").z"), "{}", wgsl.wgsl);
}

#[test]
fn sm3_verify_rejects_break_outside_loop() {
    let words = assemble_ps3_break_outside_loop();
    let decoded = crate::sm3::decode_u32_tokens(&words).expect("decode");
    let ir = crate::sm3::build_ir(&decoded).expect("build_ir");
    let err = crate::sm3::verify_ir(&ir).unwrap_err();
    assert_eq!(err.message, "break outside of a loop");
}

#[test]
fn sm3_verify_rejects_breakc_outside_loop() {
    let words = assemble_ps3_breakc_outside_loop();
    let decoded = crate::sm3::decode_u32_tokens(&words).expect("decode");
    let ir = crate::sm3::build_ir(&decoded).expect("build_ir");
    let err = crate::sm3::verify_ir(&ir).unwrap_err();
    assert_eq!(err.message, "breakc outside of a loop");
}

#[test]
fn sm3_verify_rejects_texkill_in_vertex_shader() {
    let words = assemble_vs3_texkill();
    let decoded = crate::sm3::decode_u32_tokens(&words).expect("decode");
    let ir = crate::sm3::build_ir(&decoded).expect("build_ir");
    let err = crate::sm3::verify_ir(&ir).unwrap_err();
    assert_eq!(
        err.message,
        "discard/texkill is only valid in pixel shaders"
    );
}

#[test]
fn sm3_verify_rejects_misctype_in_vertex_shader() {
    let words = assemble_vs3_reads_vpos_misc_sm3_decoder();
    let decoded = crate::sm3::decode_u32_tokens(&words).expect("decode");
    let ir = crate::sm3::build_ir(&decoded).expect("build_ir");
    let err = crate::sm3::verify_ir(&ir).unwrap_err();
    assert_eq!(
        err.message,
        "MiscType (vPos/vFace) inputs are only supported in pixel shaders"
    );
}

#[test]
fn sm3_verify_allows_texkill_in_pixel_shader() {
    let words = assemble_ps3_texkill();
    let decoded = crate::sm3::decode_u32_tokens(&words).expect("decode");
    let ir = crate::sm3::build_ir(&decoded).expect("build_ir");
    crate::sm3::verify_ir(&ir).expect("verify_ir");
}

#[test]
fn sm3_verify_rejects_unknown_misctype_index() {
    let words = assemble_ps3_reads_misc2_sm3_decoder();
    let decoded = crate::sm3::decode_u32_tokens(&words).expect("decode");
    let ir = crate::sm3::build_ir(&decoded).expect("build_ir");
    let err = crate::sm3::verify_ir(&ir).unwrap_err();
    assert_eq!(
        err.message,
        "unsupported MiscType input misc2 (only misc0=vPos and misc1=vFace are supported)"
    );
}

#[test]
fn sm3_verify_rejects_loop_init_with_non_loop_register() {
    let ir = sm3::ShaderIr {
        version: sm3::ShaderVersion {
            stage: sm3::ShaderStage::Pixel,
            major: 3,
            minor: 0,
        },
        inputs: Vec::new(),
        outputs: Vec::new(),
        samplers: Vec::new(),
        const_defs_f32: Vec::new(),
        const_defs_i32: Vec::new(),
        const_defs_bool: Vec::new(),
        subroutines: std::collections::BTreeMap::new(),
        body: sm3::ir::Block {
            stmts: vec![sm3::ir::Stmt::Loop {
                init: sm3::ir::LoopInit {
                    loop_reg: sm3::ir::RegRef {
                        file: sm3::ir::RegFile::Temp,
                        index: 0,
                        relative: None,
                    },
                    ctrl_reg: sm3::ir::RegRef {
                        file: sm3::ir::RegFile::ConstInt,
                        index: 0,
                        relative: None,
                    },
                },
                body: sm3::ir::Block::new(),
            }],
        },
        uses_semantic_locations: false,
    };
    let err = crate::sm3::verify_ir(&ir).unwrap_err();
    assert_eq!(err.message, "loop init refers to a non-loop register");
}

#[test]
fn sm3_verify_rejects_loop_init_with_non_integer_ctrl_reg() {
    let ir = sm3::ShaderIr {
        version: sm3::ShaderVersion {
            stage: sm3::ShaderStage::Pixel,
            major: 3,
            minor: 0,
        },
        inputs: Vec::new(),
        outputs: Vec::new(),
        samplers: Vec::new(),
        const_defs_f32: Vec::new(),
        const_defs_i32: Vec::new(),
        const_defs_bool: Vec::new(),
        subroutines: std::collections::BTreeMap::new(),
        body: sm3::ir::Block {
            stmts: vec![sm3::ir::Stmt::Loop {
                init: sm3::ir::LoopInit {
                    loop_reg: sm3::ir::RegRef {
                        file: sm3::ir::RegFile::Loop,
                        index: 0,
                        relative: None,
                    },
                    ctrl_reg: sm3::ir::RegRef {
                        file: sm3::ir::RegFile::Const,
                        index: 0,
                        relative: None,
                    },
                },
                body: sm3::ir::Block::new(),
            }],
        },
        uses_semantic_locations: false,
    };
    let err = crate::sm3::verify_ir(&ir).unwrap_err();
    assert_eq!(
        err.message,
        "loop init refers to a non-integer-constant register"
    );
}

#[test]
fn supports_shader_model_3() {
    let vs_bytes = to_bytes(&assemble_vs_passthrough_sm3());
    let program = shader::parse(&vs_bytes).unwrap();
    assert_eq!(program.version.model.major, 3);
}

#[test]
fn rejects_unsupported_shader_model_minor_legacy_translator() {
    // D3D9 only defines 2.0/2.1 and 3.0.
    let bytes = to_bytes(&[0xFFFF_0301, 0x0000_FFFF]);
    let err = shader::parse(&bytes).unwrap_err();
    assert!(
        matches!(err, shader::ShaderError::UnsupportedVersion(_)),
        "{err:?}"
    );
}

#[test]
fn rejects_missing_end_token_legacy_translator() {
    // Version token only, with no terminating `end` instruction.
    let bytes = to_bytes(&[0xFFFE0200]);
    let err = shader::parse(&bytes).unwrap_err();
    assert!(matches!(err, shader::ShaderError::UnexpectedEof), "{err:?}");
}

#[test]
fn rejects_excessive_control_flow_nesting_legacy_translator() {
    // Craft a ps_3_0 shader with >MAX_D3D9_SHADER_CONTROL_FLOW_NESTING nested `if` blocks.
    let mut words = vec![0xFFFF0300];

    // Nest one past the limit.
    for _ in 0..=MAX_D3D9_SHADER_CONTROL_FLOW_NESTING {
        // if b0
        words.extend(enc_inst(0x0028, &[enc_src(14, 0, 0x00)]));
    }
    for _ in 0..=MAX_D3D9_SHADER_CONTROL_FLOW_NESTING {
        // endif
        words.extend(enc_inst(0x002B, &[]));
    }
    words.push(0x0000FFFF);

    let err = shader::parse(&to_bytes(&words)).unwrap_err();
    match err {
        shader::ShaderError::InvalidControlFlow(msg) => {
            assert_eq!(msg, "control flow nesting exceeds maximum");
        }
        other => panic!("expected InvalidControlFlow, got {other:?}"),
    }
}

#[test]
fn rejects_oversized_shader_bytecode_legacy_translator() {
    // Ensure the legacy token-stream translator rejects oversized inputs without trying to
    // allocate a gigantic `Vec<u32>`.
    let bytes = vec![0u8; MAX_D3D9_SHADER_BYTECODE_BYTES + 4];
    let err = shader::parse(&bytes).unwrap_err();
    assert!(
        matches!(err, shader::ShaderError::BytecodeTooLarge { .. }),
        "{err:?}"
    );
}

#[test]
fn rejects_oversized_shader_blob() {
    // DXBC containers (and other outer shader blobs) are hashed and can be copied across the
    // wasm32 JS boundary for persistent caching. Reject absurdly large blobs early.
    let bytes = vec![0u8; MAX_D3D9_SHADER_BLOB_BYTES + 1];
    let err = shader::parse(&bytes).unwrap_err();
    assert!(
        matches!(
            err,
            shader::ShaderError::BytecodeTooLarge {
                max: MAX_D3D9_SHADER_BLOB_BYTES,
                ..
            }
        ),
        "{err:?}"
    );
}

#[test]
fn rejects_oversized_shader_bytecode_sm3_decoder() {
    // Ensure the SM3 decoder rejects oversized inputs without allocating.
    let bytes = vec![0u8; MAX_D3D9_SHADER_BYTECODE_BYTES + 4];
    let err = crate::sm3::decode_u8_le_bytes(&bytes).unwrap_err();
    assert_eq!(err.token_index, 0);
    assert!(err.message.contains("exceeds maximum"), "{}", err);
}

#[test]
fn translate_entrypoint_rejects_oversized_shader_bytecode() {
    // The high-level translation entrypoint should reject oversized token streams before attempting
    // any decoding/normalization work.
    let bytecode = vec![0u8; MAX_D3D9_SHADER_BYTECODE_BYTES + 4];

    let err =
        shader_translate::translate_d3d9_shader_to_wgsl(&bytecode, shader::WgslOptions::default())
            .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );

    // Ensure the same size cap is enforced when the bytecode is wrapped in a DXBC container.
    let dxbc = dxbc_test_utils::build_container(&[(DxbcFourCC(*b"SHDR"), &bytecode)]);
    let err =
        shader_translate::translate_d3d9_shader_to_wgsl(&dxbc, shader::WgslOptions::default())
            .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Malformed(_)),
        "{err:?}"
    );
}

#[test]
fn rejects_out_of_range_sampler_register_legacy_translator() {
    // ps_2_0 texld r0, t0, s16 (sampler index out of range).
    let mut out = vec![0xFFFF0200];
    out.extend(enc_inst(
        0x0042,
        &[
            enc_dst(0, 0, 0xF),
            enc_src(3, 0, 0xE4),
            enc_src(10, 16, 0xE4),
        ],
    ));
    out.push(0x0000FFFF);
    let bytes = to_bytes(&out);

    let err = shader::parse(&bytes).unwrap_err();
    assert!(
        matches!(
            err,
            shader::ShaderError::RegisterIndexTooLarge {
                file: shader::RegisterFile::Sampler,
                ..
            }
        ),
        "{err:?}"
    );
}

#[test]
fn rejects_malformed_lrp_with_too_few_operands_legacy_translator() {
    // ps_3_0 lrp with a bogus instruction length nibble (only dst + 2 src operands).
    // This should be rejected instead of panicking later in WGSL generation.
    let mut out = vec![0xFFFF0300];
    out.push(0x0012 | (4u32 << 24)); // opcode=0x12, total length=4 tokens (opcode + 3 params)
    out.push(enc_dst(0, 0, 0xF)); // r0
    out.push(enc_src(2, 0, 0xE4)); // c0
    out.push(enc_src(2, 1, 0xE4)); // c1 (missing c2)
    out.push(0x0000FFFF);

    let err = shader::parse(&to_bytes(&out)).unwrap_err();
    // The parser should reject malformed instruction streams (either via structural EOF checks or
    // by failing to decode a valid operand token for the missing parameter).
    assert!(
        matches!(
            err,
            shader::ShaderError::UnexpectedEof | shader::ShaderError::UnsupportedRegisterType(_)
        ),
        "{err:?}"
    );
}

#[test]
fn dxbc_container_rejects_excessive_chunk_count() {
    use crate::shader_limits::MAX_D3D9_DXBC_CHUNK_COUNT;

    // Start with a valid empty DXBC container, then patch the declared chunk count to exceed our
    // hard cap. `extract_shader_bytecode` should reject this *before* attempting to validate an
    // enormous offset table.
    let mut bytes = dxbc_test_utils::build_container(&[]);
    bytes[28..32].copy_from_slice(&(MAX_D3D9_DXBC_CHUNK_COUNT + 1).to_le_bytes()); // chunk count

    let err = dxbc::extract_shader_bytecode(&bytes).unwrap_err();
    assert!(
        matches!(err, dxbc::DxbcError::ChunkCountTooLarge { .. }),
        "{err:?}"
    );
}

#[test]
fn dxbc_container_does_not_panic_on_huge_chunk_offset() {
    // On 32-bit targets (notably wasm32), `usize` arithmetic can overflow when parsing a DXBC
    // container that includes absurd chunk offsets. Ensure we return an error instead of panicking.
    let mut bytes = dxbc_test_utils::build_container(&[(DxbcFourCC(*b"JUNK"), &[][..])]);
    // Offset table starts immediately after the fixed header (32 bytes) for a single-chunk
    // container.
    bytes[32..36].copy_from_slice(&u32::MAX.to_le_bytes()); // chunk offset

    let result = std::panic::catch_unwind(|| dxbc::extract_shader_bytecode(&bytes));
    assert!(result.is_ok(), "extract_shader_bytecode panicked");
    let err = result.unwrap().unwrap_err();
    assert!(matches!(err, dxbc::DxbcError::Shared(_)), "{err:?}");
}

#[test]
fn accepts_cube_sampler_declarations() {
    // ps_3_0 with a `dcl_cube s0` declaration.
    let mut words = vec![0xFFFF_0300];
    // Texture type = cube (3) encoded in opcode_token[16..20].
    words.extend(enc_inst_with_extra(
        0x001F,
        3u32 << 16,
        &[enc_dst(10, 0, 0xF)],
    ));
    words.push(0x0000_FFFF);

    let bytes = to_bytes(&words);
    let program = shader::parse(&bytes).unwrap();
    assert_eq!(
        program.sampler_texture_types.get(&0).copied(),
        Some(TextureType::TextureCube)
    );
}

#[test]
fn accepts_1d_sampler_declarations() {
    // ps_3_0 with a `dcl_1d s0` declaration.
    let mut words = vec![0xFFFF_0300];
    // Texture type = 1d (1) encoded in opcode_token[16..20].
    words.extend(enc_inst_with_extra(
        0x001F,
        1u32 << 16,
        &[enc_dst(10, 0, 0xF)],
    ));
    words.push(0x0000_FFFF);

    let bytes = to_bytes(&words);
    let program = shader::parse(&bytes).unwrap();
    assert_eq!(
        program.sampler_texture_types.get(&0).copied(),
        Some(TextureType::Texture1D)
    );
}

#[test]
fn accepts_volume_sampler_declarations() {
    // ps_3_0 with a `dcl_volume s0` (3d) declaration.
    let mut words = vec![0xFFFF_0300];
    // Texture type = volume (4) encoded in opcode_token[16..20].
    words.extend(enc_inst_with_extra(
        0x001F,
        4u32 << 16,
        &[enc_dst(10, 0, 0xF)],
    ));
    words.push(0x0000_FFFF);

    let bytes = to_bytes(&words);
    let program = shader::parse(&bytes).unwrap();
    assert_eq!(
        program.sampler_texture_types.get(&0).copied(),
        Some(TextureType::Texture3D)
    );
}

#[test]
fn translate_entrypoint_allows_unused_volume_sampler_declaration() {
    // Regression test: shaders can declare sampler texture types (including unknown encodings) as
    // long as the sampler is never actually used by a texture op. The WGSL backends only emit
    // bindings for used samplers.
    //
    // Minimal ps_3_0:
    //   dcl_volume s0
    //   def c0, 1, 0, 0, 1
    //   mov oC0, c0
    //   end
    let mut words = vec![0xFFFF_0300];
    // dcl_volume s0
    words.extend(enc_inst_with_extra(
        0x001F,
        4u32 << 16,
        &[enc_dst(10, 0, 0xF)],
    ));
    // def c0, 1.0, 0.0, 0.0, 1.0
    words.extend(enc_inst(
        0x0051,
        &[
            enc_dst(2, 0, 0xF),
            1.0f32.to_bits(),
            0.0f32.to_bits(),
            0.0f32.to_bits(),
            1.0f32.to_bits(),
        ],
    ));
    // mov oC0, c0
    words.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(2, 0, 0xE4)]));
    words.push(0x0000_FFFF);

    let bytes = to_bytes(&words);
    let translated =
        shader_translate::translate_d3d9_shader_to_wgsl(&bytes, shader::WgslOptions::default())
            .unwrap();
    assert!(!translated.used_samplers.contains(&0));
    assert!(
        !translated.wgsl.contains("texture_3d"),
        "wgsl:\n{}",
        translated.wgsl
    );
    assert!(
        !translated.wgsl.contains("textureSample"),
        "wgsl:\n{}",
        translated.wgsl
    );
}

#[test]
fn translate_entrypoint_rejects_used_volume_sampler() {
    // The D3D9 runtime currently only supports 2D + cube textures. Ensure we reject used
    // volume (3D) samplers at the translation entrypoint rather than emitting WGSL that would
    // fail WebGPU validation once bindings are created.
    let mut words = vec![0xFFFF_0300];
    // dcl_volume s0
    words.extend(enc_inst_with_extra(
        0x001F,
        4u32 << 16,
        &[enc_dst(10, 0, 0xF)],
    ));
    // texld r0, t0, s0
    words.extend(enc_inst(
        0x0042,
        &[
            enc_dst(0, 0, 0xF),
            enc_src(3, 0, 0xE4),
            enc_src(10, 0, 0xE4),
        ],
    ));
    // mov oC0, r0
    words.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    words.push(0x0000_FFFF);

    let bytes = to_bytes(&words);
    let err =
        shader_translate::translate_d3d9_shader_to_wgsl(&bytes, shader::WgslOptions::default())
            .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Translation(_)),
        "{err:?}"
    );
    let msg = err.to_string();
    assert!(msg.contains("Texture3D") && msg.contains("s0"), "{msg}");
}

#[test]
fn translate_entrypoint_rejects_used_1d_sampler() {
    // Used 1D samplers are rejected by the translation entrypoint because the D3D9 command stream
    // cannot currently bind 1D resources. (Unused declarations are still accepted.)
    let mut words = vec![0xFFFF_0300];
    // dcl_1d s0
    words.extend(enc_inst_with_extra(
        0x001F,
        1u32 << 16,
        &[enc_dst(10, 0, 0xF)],
    ));
    // texld r0, t0, s0
    words.extend(enc_inst(
        0x0042,
        &[
            enc_dst(0, 0, 0xF),
            enc_src(3, 0, 0xE4),
            enc_src(10, 0, 0xE4),
        ],
    ));
    // mov oC0, r0
    words.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    words.push(0x0000_FFFF);

    let bytes = to_bytes(&words);
    let err =
        shader_translate::translate_d3d9_shader_to_wgsl(&bytes, shader::WgslOptions::default())
            .unwrap_err();
    assert!(
        matches!(err, shader_translate::ShaderTranslateError::Translation(_)),
        "{err:?}"
    );
    let msg = err.to_string();
    assert!(msg.contains("Texture1D") && msg.contains("s0"), "{msg}");
}

#[test]
fn legacy_translator_emits_texture_1d_and_x_coords() {
    // Minimal ps_3_0 that samples from a 1D texture.
    let mut words = vec![0xFFFF_0300];
    // dcl_1d s0
    words.extend(enc_inst_with_extra(
        0x001F,
        1u32 << 16,
        &[enc_dst(10, 0, 0xF)],
    ));
    // texld r0, t0, s0
    words.extend(enc_inst(
        0x0042,
        &[
            enc_dst(0, 0, 0xF),
            enc_src(3, 0, 0xE4),
            enc_src(10, 0, 0xE4),
        ],
    ));
    // mov oC0, r0
    words.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    words.push(0x0000_FFFF);

    let bytes = to_bytes(&words);
    let program = shader::parse(&bytes).unwrap();
    let ir = shader::to_ir(&program);
    let wgsl = shader::generate_wgsl(&ir).unwrap();

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(
        wgsl.wgsl.contains("texture_1d<f32>"),
        "wgsl:\n{}",
        wgsl.wgsl
    );
    assert!(
        wgsl.wgsl
            .contains("textureSample(tex0, samp0, (t0.xyzw).x)"),
        "wgsl:\n{}",
        wgsl.wgsl
    );
}

#[test]
fn legacy_translator_emits_texture_3d_and_xyz_coords() {
    // Minimal ps_3_0 that samples from a 3D texture.
    let mut words = vec![0xFFFF_0300];
    // dcl_volume s0
    words.extend(enc_inst_with_extra(
        0x001F,
        4u32 << 16,
        &[enc_dst(10, 0, 0xF)],
    ));
    // texld r0, t0, s0
    words.extend(enc_inst(
        0x0042,
        &[
            enc_dst(0, 0, 0xF),
            enc_src(3, 0, 0xE4),
            enc_src(10, 0, 0xE4),
        ],
    ));
    // mov oC0, r0
    words.extend(enc_inst(0x0001, &[enc_dst(8, 0, 0xF), enc_src(0, 0, 0xE4)]));
    words.push(0x0000_FFFF);

    let bytes = to_bytes(&words);
    let program = shader::parse(&bytes).unwrap();
    let ir = shader::to_ir(&program);
    let wgsl = shader::generate_wgsl(&ir).unwrap();

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(
        wgsl.wgsl.contains("texture_3d<f32>"),
        "wgsl:\n{}",
        wgsl.wgsl
    );
    assert!(
        wgsl.wgsl
            .contains("textureSample(tex0, samp0, (t0.xyzw).xyz)"),
        "wgsl:\n{}",
        wgsl.wgsl
    );
}

fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

#[test]
fn parses_isgn_signature_chunk() {
    // Minimal ISGN-like chunk with a single POSITION element.
    let chunk = dxbc_test_utils::build_signature_chunk_v0(&[dxbc_test_utils::SignatureEntryDesc {
        semantic_name: "POSITION",
        semantic_index: 0,
        system_value_type: 0,
        component_type: 0,
        register: 0,
        mask: 0xF,
        read_write_mask: 0xF,
        stream: 0,
        min_precision: 0,
    }]);

    let sig = parse_signature_chunk(&chunk).unwrap();
    assert_eq!(sig.entries.len(), 1);
    assert_eq!(sig.entries[0].semantic_name, "POSITION");
    assert_eq!(sig.entries[0].semantic_index, 0);
    assert_eq!(sig.entries[0].register, 0);
    assert_eq!(sig.entries[0].mask, 0xF);
}

#[test]
fn parses_rdef_resource_bindings() {
    // Minimal RDEF-like chunk with a single texture bound at t3.
    let mut chunk = Vec::new();
    push_u32(&mut chunk, 0); // cb count
    push_u32(&mut chunk, 0); // cb offset
    push_u32(&mut chunk, 1); // resource count
    push_u32(&mut chunk, 28); // resource offset (header size)
    push_u32(&mut chunk, 0); // shader model
    push_u32(&mut chunk, 0); // flags
    push_u32(&mut chunk, 0); // creator offset

    // Resource entry (32 bytes).
    push_u32(&mut chunk, 60); // name offset
    push_u32(&mut chunk, 0); // type
    push_u32(&mut chunk, 0); // return type
    push_u32(&mut chunk, 0); // dimension
    push_u32(&mut chunk, 0); // num samples
    push_u32(&mut chunk, 3); // bind point
    push_u32(&mut chunk, 1); // bind count
    push_u32(&mut chunk, 0); // flags

    chunk.extend_from_slice(b"tex0\0");

    let rdef = parse_rdef_chunk(&chunk).unwrap();
    assert_eq!(rdef.bound_resources.len(), 1);
    assert_eq!(rdef.bound_resources[0].name, "tex0");
    assert_eq!(rdef.bound_resources[0].bind_point, 3);
}

#[test]
fn parses_ctab_constant_table() {
    // Minimal CTAB chunk with a single constant c0 and target string.
    let mut chunk = Vec::new();
    push_u32(&mut chunk, 0); // size (ignored)
    push_u32(&mut chunk, 0); // creator offset
    push_u32(&mut chunk, 0); // version
    push_u32(&mut chunk, 1); // constant count
    push_u32(&mut chunk, 28); // constant info offset
    push_u32(&mut chunk, 0); // flags
    push_u32(&mut chunk, 48); // target offset (after entry)

    // Constant info entry (20 bytes).
    push_u32(&mut chunk, 55); // name offset (after target string)
    push_u16(&mut chunk, 0); // register set
    push_u16(&mut chunk, 0); // register index
    push_u16(&mut chunk, 1); // register count
    push_u16(&mut chunk, 0); // reserved
    push_u32(&mut chunk, 0); // type info offset
    push_u32(&mut chunk, 0); // default value offset

    chunk.extend_from_slice(b"ps_2_0\0"); // 7 bytes -> next offset 55
    chunk.extend_from_slice(b"C0\0");

    let ctab = parse_ctab_chunk(&chunk).unwrap();
    assert_eq!(ctab.target.as_deref(), Some("ps_2_0"));
    assert_eq!(ctab.constants.len(), 1);
    assert_eq!(ctab.constants[0].name, "C0");
    assert_eq!(ctab.constants[0].register_index, 0);
    assert_eq!(ctab.constants[0].register_count, 1);
}

#[test]
fn converts_guest_textures_to_rgba8() {
    let rgba = state::convert_guest_texture_to_rgba8(
        state::TextureFormat::A8R8G8B8,
        1,
        1,
        4,
        &[0x01, 0x02, 0x03, 0x04], // BGRA
    );
    assert_eq!(rgba, vec![0x03, 0x02, 0x01, 0x04]);

    let rgba = state::convert_guest_texture_to_rgba8(
        state::TextureFormat::X8R8G8B8,
        1,
        1,
        4,
        &[0x10, 0x20, 0x30, 0x00], // BGRX
    );
    assert_eq!(rgba, vec![0x30, 0x20, 0x10, 0xFF]);

    let rgba = state::convert_guest_texture_to_rgba8(state::TextureFormat::A8, 1, 1, 1, &[0x7F]);
    assert_eq!(rgba, vec![0xFF, 0xFF, 0xFF, 0x7F]);
}
