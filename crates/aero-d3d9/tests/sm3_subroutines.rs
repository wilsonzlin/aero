use std::collections::HashMap;

use aero_d3d9::sm3::types::ShaderStage;
use aero_d3d9::sm3::{build_ir, decode_u32_tokens, generate_wgsl, verify_ir};
use aero_d3d9::state::{
    BlendState, SamplerState, VertexDecl, VertexElement, VertexElementType, VertexUsage,
};
use aero_d3d9::{sm3, software};

fn version_token(stage: ShaderStage, major: u8, minor: u8) -> u32 {
    let prefix = match stage {
        ShaderStage::Vertex => 0xFFFE_0000,
        ShaderStage::Pixel => 0xFFFF_0000,
    };
    prefix | ((major as u32) << 8) | (minor as u32)
}

fn opcode_token(op: u16, operand_count: u8) -> u32 {
    // D3D9 SM2/SM3 encodes the *total* instruction length in tokens (including the opcode token)
    // in bits 24..27.
    (op as u32) | (((operand_count as u32) + 1) << 24)
}

fn reg_token(regtype: u8, index: u32) -> u32 {
    let low3 = (regtype as u32) & 0x7;
    let high2 = (regtype as u32) & 0x18;
    0x8000_0000 | (low3 << 28) | (high2 << 8) | (index & 0x7FF)
}

fn dst_token(regtype: u8, index: u32, mask: u8) -> u32 {
    reg_token(regtype, index) | ((mask as u32) << 16)
}

fn src_token(regtype: u8, index: u32, swizzle: u8, srcmod: u8) -> u32 {
    reg_token(regtype, index) | ((swizzle as u32) << 16) | ((srcmod as u32) << 24)
}

fn push_vec4(buf: &mut Vec<u8>, v: software::Vec4) {
    buf.extend_from_slice(&v.x.to_le_bytes());
    buf.extend_from_slice(&v.y.to_le_bytes());
    buf.extend_from_slice(&v.z.to_le_bytes());
    buf.extend_from_slice(&v.w.to_le_bytes());
}

fn build_fullscreen_triangle_vb() -> Vec<u8> {
    let mut vb = Vec::new();
    // Fullscreen triangle in clip space.
    for v in [
        software::Vec4::new(-1.0, -1.0, 0.0, 1.0),
        software::Vec4::new(3.0, -1.0, 0.0, 1.0),
        software::Vec4::new(-1.0, 3.0, 0.0, 1.0),
    ] {
        push_vec4(&mut vb, v);
    }
    vb
}

fn build_pos_only_decl() -> VertexDecl {
    VertexDecl::new(
        16,
        vec![VertexElement {
            offset: 0,
            ty: VertexElementType::Float4,
            usage: VertexUsage::Position,
            usage_index: 0,
        }],
    )
}

fn build_vs_passthrough() -> sm3::ShaderIr {
    // vs_2_0:
    //   mov oPos, v0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Vertex, 2, 0),
        opcode_token(1, 2),
        dst_token(4, 0, 0xF),     // oPos
        src_token(1, 0, 0xE4, 0), // v0
        0x0000_FFFF,
    ];
    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();
    ir
}

#[test]
fn sm3_call_and_label_execute_in_software_and_wgsl_compiles() {
    // ps_3_0:
    //   mov r0, c0
    //   call l0
    //   mov oC0, r0
    //   ret
    //   label l0
    //   mov r0, c1
    //   ret
    //   end
    let ps_tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // mov r0, c0
        opcode_token(1, 2),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        // call l0
        opcode_token(25, 1),
        src_token(18, 0, 0xE4, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // ret (end main)
        opcode_token(28, 0),
        // label l0
        opcode_token(30, 1),
        src_token(18, 0, 0xE4, 0),
        // mov r0, c1
        opcode_token(1, 2),
        dst_token(0, 0, 0xF),
        src_token(2, 1, 0xE4, 0),
        // ret (return from subroutine)
        opcode_token(28, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&ps_tokens).unwrap();
    let ps = build_ir(&decoded).unwrap();
    verify_ir(&ps).unwrap();

    // Ensure WGSL generation succeeds and produces a valid module.
    let wgsl = generate_wgsl(&ps).unwrap().wgsl;
    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    let vs = build_vs_passthrough();
    let decl = build_pos_only_decl();
    let vb = build_fullscreen_triangle_vb();

    let mut constants = [software::Vec4::ZERO; 256];
    constants[0] = software::Vec4::new(1.0, 0.0, 0.0, 1.0); // c0 = red
    constants[1] = software::Vec4::new(0.0, 1.0, 0.0, 1.0); // c1 = green

    let mut rt = software::RenderTarget::new(4, 4, software::Vec4::ZERO);
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
            sampler_states: &HashMap::<u16, SamplerState>::new(),
            blend_state: BlendState::default(),
        },
    );

    // Expect green: subroutine overwrote r0 before final write.
    assert_eq!(rt.get(2, 2).to_rgba8(), [0, 255, 0, 255]);
}

#[test]
fn sm3_nonuniform_conditional_call_to_derivative_subroutine_is_naga_valid() {
    // ps_3_0:
    //   def c0, 0.0, 0.0, 0.0, 0.0
    //   mov r0, c0
    //   setp_ne p0, v0.x, c0.x
    //   callnz (p0) l0, v0
    //   mov oC0, r0
    //   ret
    //   label l0
    //   dsx r0, v0
    //   ret
    //   end
    //
    // This is deliberately non-uniform control flow (predicate depends on `v0.x`) with a
    // derivative op inside the called subroutine. WGSL requires derivative ops to be in uniform
    // control flow, so the lowering must avoid emitting `if (cond) { aero_sub(); }` in this case.
    let ps_tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // def c0, 0.0, 0.0, 0.0, 0.0
        opcode_token(81, 5),
        dst_token(2, 0, 0xF),
        0x0000_0000,
        0x0000_0000,
        0x0000_0000,
        0x0000_0000,
        // mov r0, c0
        opcode_token(1, 2),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        // setp_ne p0, v0.x, c0.x  (compare op 4 = ne)
        opcode_token(94, 3) | (4u32 << 16),
        dst_token(19, 0, 0xF),
        src_token(1, 0, 0x00, 0), // v0.xxxx
        src_token(2, 0, 0x00, 0), // c0.xxxx
        // callnz (p0) l0, v0
        opcode_token(26, 3) | 0x1000_0000, // predicated
        src_token(18, 0, 0xE4, 0),
        src_token(1, 0, 0xE4, 0),
        src_token(19, 0, 0x00, 0), // p0.x
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // ret (end main)
        opcode_token(28, 0),
        // label l0
        opcode_token(30, 1),
        src_token(18, 0, 0xE4, 0),
        // dsx r0, v0
        opcode_token(86, 2),
        dst_token(0, 0, 0xF),
        src_token(1, 0, 0xE4, 0),
        // ret
        opcode_token(28, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&ps_tokens).unwrap();
    let ps = build_ir(&decoded).unwrap();
    verify_ir(&ps).unwrap();

    let wgsl = generate_wgsl(&ps).unwrap().wgsl;
    assert!(wgsl.contains("dpdx("), "{wgsl}");
    assert!(wgsl.contains("_aero_call_taken_"), "{wgsl}");
    assert!(wgsl.contains("_aero_saved_call"), "{wgsl}");
    // The call should not remain under a non-uniform `if`.
    assert!(!wgsl.contains("if (p0.x)"), "{wgsl}");
    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn sm3_callnz_is_conditional_in_software() {
    // ps_3_0:
    //   mov r0, c0
    //   callnz l0, c2
    //   mov oC0, r0
    //   ret
    //   label l0
    //   mov r0, c1
    //   ret
    //   end
    let ps_tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // mov r0, c0
        opcode_token(1, 2),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        // callnz l0, c2
        opcode_token(26, 2),
        src_token(18, 0, 0xE4, 0),
        src_token(2, 2, 0xE4, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // ret (end main)
        opcode_token(28, 0),
        // label l0
        opcode_token(30, 1),
        src_token(18, 0, 0xE4, 0),
        // mov r0, c1
        opcode_token(1, 2),
        dst_token(0, 0, 0xF),
        src_token(2, 1, 0xE4, 0),
        // ret
        opcode_token(28, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&ps_tokens).unwrap();
    let ps = build_ir(&decoded).unwrap();
    verify_ir(&ps).unwrap();
    let vs = build_vs_passthrough();
    let decl = build_pos_only_decl();
    let vb = build_fullscreen_triangle_vb();

    // Case 1: c2.x == 0 -> callnz not taken -> output red.
    let mut constants = [software::Vec4::ZERO; 256];
    constants[0] = software::Vec4::new(1.0, 0.0, 0.0, 1.0); // red
    constants[1] = software::Vec4::new(0.0, 1.0, 0.0, 1.0); // green
    constants[2] = software::Vec4::new(0.0, 0.0, 0.0, 0.0);

    let mut rt = software::RenderTarget::new(4, 4, software::Vec4::ZERO);
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
            sampler_states: &HashMap::<u16, SamplerState>::new(),
            blend_state: BlendState::default(),
        },
    );
    assert_eq!(rt.get(2, 2).to_rgba8(), [255, 0, 0, 255]);

    // Case 2: c2.x != 0 -> callnz taken -> output green.
    constants[2] = software::Vec4::new(1.0, 0.0, 0.0, 0.0);
    let mut rt = software::RenderTarget::new(4, 4, software::Vec4::ZERO);
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
            sampler_states: &HashMap::<u16, SamplerState>::new(),
            blend_state: BlendState::default(),
        },
    );
    assert_eq!(rt.get(2, 2).to_rgba8(), [0, 255, 0, 255]);
}

#[test]
fn sm3_call_missing_label_is_an_error() {
    // ps_3_0:
    //   call l0
    //   ret
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        opcode_token(25, 1),
        src_token(18, 0, 0xE4, 0),
        opcode_token(28, 0),
        0x0000_FFFF,
    ];
    let decoded = decode_u32_tokens(&tokens).unwrap();
    let err = build_ir(&decoded).unwrap_err();
    assert!(
        err.message.contains("call target label l0 is not defined"),
        "{err}"
    );
}

#[test]
fn sm3_excessive_call_depth_is_an_error() {
    // Build a deep non-recursive call chain: main -> l0 -> l1 -> ... -> l63.
    // This should exceed the translator's hard cap and error deterministically.
    let mut tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // call l0
        opcode_token(25, 1),
        src_token(18, 0, 0xE4, 0),
        // ret (end main)
        opcode_token(28, 0),
    ];

    for i in 0..64u32 {
        // label li
        tokens.push(opcode_token(30, 1));
        tokens.push(src_token(18, i, 0xE4, 0));
        if i < 63 {
            // call l(i+1)
            tokens.push(opcode_token(25, 1));
            tokens.push(src_token(18, i + 1, 0xE4, 0));
        }
        // ret
        tokens.push(opcode_token(28, 0));
    }
    tokens.push(0x0000_FFFF);

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let err = build_ir(&decoded).unwrap_err();
    assert!(
        err.message
            .contains("subroutine call stack depth exceeds maximum"),
        "{err}"
    );
}
