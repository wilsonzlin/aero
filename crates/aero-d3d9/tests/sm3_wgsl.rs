use aero_d3d9::sm3::decode::Opcode;
use aero_d3d9::sm3::types::ShaderStage;
use aero_d3d9::sm3::{build_ir, decode_u32_tokens, generate_wgsl, verify_ir};

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

#[test]
fn wgsl_vs20_reads_v0_writes_opos_compiles() {
    // vs_2_0:
    //   mov oPos, v0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Vertex, 2, 0),
        opcode_token(1, 2),
        dst_token(4, 0, 0xF),          // oPos
        src_token(1, 0, 0xE4, 0),      // v0
        0x0000_FFFF,                   // end
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(wgsl.contains("struct VsInput"), "{wgsl}");
    assert!(wgsl.contains("@location(0) v0"), "{wgsl}");
    assert!(wgsl.contains("@builtin(position)"), "{wgsl}");
}

#[test]
fn wgsl_ps20_reads_t0_and_v0_compiles() {
    // ps_2_0:
    //   add r0, t0, v0
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 2, 0),
        // add r0, t0, v0
        opcode_token(2, 3),
        dst_token(0, 0, 0xF),     // r0
        src_token(3, 0, 0xE4, 0), // t0
        src_token(1, 0, 0xE4, 0), // v0
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),     // oC0
        src_token(0, 0, 0xE4, 0), // r0
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(wgsl.contains("struct FsIn"), "{wgsl}");
    assert!(wgsl.contains("@location(0) v0"), "{wgsl}");
    // Legacy mapping for t# starts at location 4.
    assert!(wgsl.contains("@location(4) t0"), "{wgsl}");
}

#[test]
fn wgsl_ps30_reads_vpos_compiles() {
    // ps_3_0:
    //   mov r0, vPos
    //   mov oC0, r0
    //   end
    //
    // D3D9 encodes vPos as a MiscType register (regtype 17, index 0).
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // mov r0, misc0
        opcode_token(1, 2),
        dst_token(0, 0, 0xF),      // r0
        src_token(17, 0, 0xE4, 0), // vPos
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),     // oC0
        src_token(0, 0, 0xE4, 0), // r0
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(wgsl.contains("@builtin(position)"), "{wgsl}");
    assert!(wgsl.contains("let misc0"), "{wgsl}");
}

#[test]
fn wgsl_ps30_reads_vface_compiles() {
    // ps_3_0:
    //   mov r0, vFace
    //   mov oC0, r0
    //   end
    //
    // D3D9 encodes vFace as a MiscType register (regtype 17, index 1).
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // mov r0, misc1
        opcode_token(1, 2),
        dst_token(0, 0, 0xF),      // r0
        src_token(17, 1, 0xE4, 0), // vFace
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),     // oC0
        src_token(0, 0, 0xE4, 0), // r0
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(wgsl.contains("@builtin(front_facing)"), "{wgsl}");
    assert!(wgsl.contains("let misc1"), "{wgsl}");
}

#[test]
fn wgsl_ps30_writes_odepth_compiles() {
    // ps_3_0:
    //   mov oDepth, c0.x
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // mov oDepth.x, c0.x
        opcode_token(1, 2),
        dst_token(9, 0, 0x1),     // oDepth.x
        src_token(2, 0, 0x00, 0), // c0.xxxx
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(wgsl.contains("@builtin(frag_depth)"), "{wgsl}");
    assert!(wgsl.contains("out.frag_depth"), "{wgsl}");
}

#[test]
fn wgsl_texld_emits_texture_sample() {
    // ps_2_0:
    //   texld r0, c0, s0
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 2, 0),
        // texld r0, c0, s0
        opcode_token(66, 3),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap();
    assert!(wgsl.wgsl.contains("textureSample("), "{}", wgsl.wgsl);
    assert_eq!(
        wgsl.bind_group_layout.sampler_bindings.get(&0),
        Some(&(1, 2))
    );

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_texldp_emits_projective_divide() {
    // ps_2_0:
    //   texldp r0, c0, s0
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 2, 0),
        // texldp r0, c0, s0 (project flag is opcode_token[16])
        opcode_token(66, 3) | (1u32 << 16),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("textureSample("), "{wgsl}");
    assert!(
        wgsl.contains("((c0).xy / (c0).w)") || wgsl.contains(").xy / (c0).w"),
        "{wgsl}"
    );

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_texldd_emits_texture_sample_grad() {
    // ps_3_0:
    //   texldd r0, c0, c1, c2, s0
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // texldd r0, c0, c1, c2, s0
        opcode_token(77, 5),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(2, 1, 0xE4, 0),
        src_token(2, 2, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    assert!(decoded
        .instructions
        .iter()
        .any(|i| i.opcode == Opcode::TexLdd));
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("textureSampleGrad("), "{wgsl}");

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_vs_texld_emits_texture_sample_level() {
    // vs_3_0:
    //   texld r0, c0, s0
    //   mov oPos, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Vertex, 3, 0),
        // texld r0, c0, s0
        opcode_token(66, 3),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // mov oPos, r0
        opcode_token(1, 2),
        dst_token(4, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("@vertex"), "{wgsl}");
    assert!(wgsl.contains("textureSampleLevel("), "{wgsl}");

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_texldl_emits_texture_sample_level_explicit_lod() {
    // ps_3_0:
    //   texldl r0, c0, s1
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // texldl r0, c0, s1
        opcode_token(79, 3),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(10, 1, 0xE4, 0),
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    assert!(decoded.instructions.iter().any(|i| i.opcode == Opcode::TexLdl));
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap();
    assert!(wgsl.wgsl.contains("textureSampleLevel("), "{}", wgsl.wgsl);
    assert!(wgsl.wgsl.contains("(c0).w"), "{}", wgsl.wgsl);
    assert_eq!(
        wgsl.bind_group_layout.sampler_bindings.get(&1),
        Some(&(3, 4))
    );

    let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_vs_texldd_is_rejected() {
    // vs_3_0:
    //   texldd r0, c0, c1, c2, s0
    //   mov oPos, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Vertex, 3, 0),
        // texldd r0, c0, c1, c2, s0
        opcode_token(77, 5),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(2, 1, 0xE4, 0),
        src_token(2, 2, 0xE4, 0),
        src_token(10, 0, 0xE4, 0),
        // mov oPos, r0
        opcode_token(1, 2),
        dst_token(4, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    assert!(decoded.instructions.iter().any(|i| i.opcode == Opcode::TexLdd));
    let ir = build_ir(&decoded).unwrap();
    let err = verify_ir(&ir).unwrap_err();
    assert!(err
        .message
        .contains("only valid in pixel shaders"), "{err}");
}

#[test]
fn wgsl_defb_if_compiles() {
    // ps_3_0:
    //   def c0, 1,0,0,1
    //   def c1, 0,1,0,1
    //   defb b0, true
    //   if b0
    //     mov oC0, c0
    //   else
    //     mov oC0, c1
    //   endif
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // def c0, 1, 0, 0, 1
        opcode_token(81, 5),
        dst_token(2, 0, 0xF),
        0x3F80_0000,
        0x0000_0000,
        0x0000_0000,
        0x3F80_0000,
        // def c1, 0, 1, 0, 1
        opcode_token(81, 5),
        dst_token(2, 1, 0xF),
        0x0000_0000,
        0x3F80_0000,
        0x0000_0000,
        0x3F80_0000,
        // defb b0, true
        opcode_token(83, 2),
        dst_token(14, 0, 0xF),
        1,
        // if b0
        opcode_token(40, 1),
        src_token(14, 0, 0x00, 0),
        // mov oC0, c0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        // else
        opcode_token(42, 0),
        // mov oC0, c1
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(2, 1, 0xE4, 0),
        // endif
        opcode_token(43, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();
    assert_eq!(ir.const_defs_bool.len(), 1);

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(
        wgsl.contains("let b0: vec4<bool> = vec4<bool>(true, true, true, true);"),
        "{wgsl}"
    );
    assert!(wgsl.contains("if ("));
}

#[test]
fn wgsl_defi_loop_breakc_compiles() {
    // ps_3_0:
    //   defi i0, 1, 0, 0, 0
    //   loop aL, i0
    //     breakc_ne i0.x, i0.y
    //   endloop
    //   mov oC0, c0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // defi i0, 1, 0, 0, 0
        opcode_token(82, 5),
        dst_token(7, 0, 0xF),
        1,
        0,
        0,
        0,
        // loop aL, i0
        opcode_token(27, 2),
        src_token(15, 0, 0xE4, 0), // aL
        src_token(7, 0, 0xE4, 0),  // i0
        // breakc_ne i0.x, i0.y  (compare op 4 = ne)
        opcode_token(45, 2) | (4u32 << 16),
        src_token(7, 0, 0x00, 0), // i0.xxxx
        src_token(7, 0, 0x55, 0), // i0.yyyy
        // endloop
        opcode_token(29, 0),
        // mov oC0, c0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();
    assert_eq!(ir.const_defs_i32.len(), 1);

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");

    assert!(
        wgsl.contains("let i0: vec4<i32> = vec4<i32>(1, 0, 0, 0);"),
        "{wgsl}"
    );
    assert!(wgsl.contains("loop {"), "{wgsl}");
    // Safety cap makes the loop structurally bounded in WGSL.
    assert!(wgsl.contains(">= 1024u"), "{wgsl}");
    assert!(wgsl.contains("if (_aero_loop_step == 0)"), "{wgsl}");
}

#[test]
fn wgsl_frc_cmp_compiles() {
    // ps_2_0:
    //   def c0, 1.25, -2.5, 3.0, -4.0
    //   def c1, 0.0, 0.0, 0.0, 1.0
    //   frc r0, c0
    //   cmp r1, c0, r0, c1
    //   mov oC0, r1
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 2, 0),
        // def c0, 1.25, -2.5, 3.0, -4.0
        opcode_token(81, 5),
        dst_token(2, 0, 0xF),
        0x3FA0_0000, // 1.25
        0xC020_0000, // -2.5
        0x4040_0000, // 3.0
        0xC080_0000, // -4.0
        // def c1, 0, 0, 0, 1
        opcode_token(81, 5),
        dst_token(2, 1, 0xF),
        0x0000_0000,
        0x0000_0000,
        0x0000_0000,
        0x3F80_0000,
        // frc r0, c0
        opcode_token(0x0013, 2),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        // cmp r1, c0, r0, c1
        opcode_token(0x0058, 4),
        dst_token(0, 1, 0xF),
        src_token(2, 0, 0xE4, 0), // cond
        src_token(0, 0, 0xE4, 0), // src_ge
        src_token(2, 1, 0xE4, 0), // src_lt
        // mov oC0, r1
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 1, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    assert!(decoded.instructions.iter().any(|i| i.opcode == Opcode::Frc));
    assert!(decoded.instructions.iter().any(|i| i.opcode == Opcode::Cmp));

    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("fract("), "{wgsl}");
    assert!(wgsl.contains("select("), "{wgsl}");

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_setp_and_predication_compiles() {
    // ps_3_0:
    //   def c0, 1,1,1,1
    //   def c1, 0,0,0,0
    //   def c2, 0.25, 0.5, 0.75, 1.0
    //   setp_ge p0, c0.x, c1.x
    //   mov (p0) oC0, c2
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // def c0, 1,1,1,1
        opcode_token(81, 5),
        dst_token(2, 0, 0xF),
        0x3F80_0000,
        0x3F80_0000,
        0x3F80_0000,
        0x3F80_0000,
        // def c1, 0,0,0,0
        opcode_token(81, 5),
        dst_token(2, 1, 0xF),
        0x0000_0000,
        0x0000_0000,
        0x0000_0000,
        0x0000_0000,
        // def c2, 0.25, 0.5, 0.75, 1.0
        opcode_token(81, 5),
        dst_token(2, 2, 0xF),
        0x3E80_0000,
        0x3F00_0000,
        0x3F40_0000,
        0x3F80_0000,
        // setp_ge p0, c0.x, c1.x  (compare op 2 = ge)
        opcode_token(78, 3) | (2u32 << 16),
        dst_token(19, 0, 0xF),
        src_token(2, 0, 0x00, 0), // c0.xxxx
        src_token(2, 1, 0x00, 0), // c1.xxxx
        // mov (p0) oC0, c2
        opcode_token(1, 3) | 0x1000_0000, // predicated
        dst_token(8, 0, 0xF),
        src_token(2, 2, 0xE4, 0),
        src_token(19, 0, 0x00, 0), // p0.x
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("var p0"), "{wgsl}");
    assert!(wgsl.contains("if ("), "{wgsl}");

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_dp2_compiles_and_uses_xy() {
    // ps_2_0:
    //   def c0, 1, 2, 3, 4
    //   def c1, 0.5, 1, 1.5, 2
    //   dp2_sat_x2 r0, c0.zwxy, -c1.yxwz
    //   mov oC0, r0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 2, 0),
        // def c0, 1, 2, 3, 4
        opcode_token(81, 5),
        dst_token(2, 0, 0xF),
        0x3F80_0000,
        0x4000_0000,
        0x4040_0000,
        0x4080_0000,
        // def c1, 0.5, 1, 1.5, 2
        opcode_token(81, 5),
        dst_token(2, 1, 0xF),
        0x3F00_0000,
        0x3F80_0000,
        0x3FC0_0000,
        0x4000_0000,
        // dp2_sat_x2 r0, c0.zwxy, -c1.yxwz
        opcode_token(90, 3) | (3u32 << 20), // modbits: saturate + mul2
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0x4E, 0), // c0.zwxy
        src_token(2, 1, 0xB1, 1), // -c1.yxwz
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    assert!(decoded.instructions.iter().any(|i| i.opcode == Opcode::Dp2));

    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("dot("), "{wgsl}");
    assert!(wgsl.contains(".xy"), "{wgsl}");
    assert!(wgsl.contains("clamp("), "{wgsl}");
    assert!(wgsl.contains("* 2.0"), "{wgsl}");

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_lrp_dp2add_compiles() {
    // ps_3_0:
    //   def c0, 0.5, 0.25, -0.5, 2.0
    //   def c1, 1.0, 2.0, 3.0, 4.0
    //   def c2, 0.0, 0.0, 0.0, 0.0
    //   lrp r0, c0, c1, c2
    //   dp2add_sat_x2 r1, c0, c1, c2
    //   mov oC0, r1
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // def c0, 0.5, 0.25, -0.5, 2.0
        opcode_token(81, 5),
        dst_token(2, 0, 0xF),
        0x3F00_0000,
        0x3E80_0000,
        0xBF00_0000,
        0x4000_0000,
        // def c1, 1, 2, 3, 4
        opcode_token(81, 5),
        dst_token(2, 1, 0xF),
        0x3F80_0000,
        0x4000_0000,
        0x4040_0000,
        0x4080_0000,
        // def c2, 0, 0, 0, 0
        opcode_token(81, 5),
        dst_token(2, 2, 0xF),
        0x0000_0000,
        0x0000_0000,
        0x0000_0000,
        0x0000_0000,
        // lrp r0, c0, c1, c2
        opcode_token(18, 4),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(2, 1, 0xE4, 0),
        src_token(2, 2, 0xE4, 0),
        // dp2add_sat_x2 r1, c0, c1, c2  (saturate + mul2)
        opcode_token(89, 4) | (3u32 << 20),
        dst_token(0, 1, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(2, 1, 0xE4, 0),
        src_token(2, 2, 0xE4, 0),
        // mov oC0, r1
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 1, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    assert!(decoded.instructions.iter().any(|i| i.opcode == Opcode::Lrp));
    assert!(decoded.instructions.iter().any(|i| i.opcode == Opcode::Dp2Add));

    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("mix("), "{wgsl}");
    assert!(wgsl.contains("dot("), "{wgsl}");
    assert!(wgsl.contains(".xy"), "{wgsl}");
    assert!(wgsl.contains(").x"), "{wgsl}");
    assert!(wgsl.contains("clamp("), "{wgsl}");
    assert!(wgsl.contains("* 2.0"), "{wgsl}");

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_dsx_dsy_derivatives_compile() {
    // ps_3_0:
    //   def c0, 0.25, 0.5, 0.75, 1.0
    //   def c1, 1.0, 1.0, 1.0, 1.0
    //   def c2, 0.0, 0.0, 0.0, 0.0
    //   setp_ge p0, c1.x, c2.x
    //   dsx (p0) r0, c0
    //   dsy_sat_x2 r1, c0
    //   add r2, r0, r1
    //   mov oC0, r2
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // def c0, 0.25, 0.5, 0.75, 1.0
        opcode_token(81, 5),
        dst_token(2, 0, 0xF),
        0x3E80_0000,
        0x3F00_0000,
        0x3F40_0000,
        0x3F80_0000,
        // def c1, 1.0, 1.0, 1.0, 1.0
        opcode_token(81, 5),
        dst_token(2, 1, 0xF),
        0x3F80_0000,
        0x3F80_0000,
        0x3F80_0000,
        0x3F80_0000,
        // def c2, 0.0, 0.0, 0.0, 0.0
        opcode_token(81, 5),
        dst_token(2, 2, 0xF),
        0x0000_0000,
        0x0000_0000,
        0x0000_0000,
        0x0000_0000,
        // setp_ge p0, c1.x, c2.x  (compare op 2 = ge)
        opcode_token(78, 3) | (2u32 << 16),
        dst_token(19, 0, 0xF),
        src_token(2, 1, 0x00, 0), // c1.xxxx
        src_token(2, 2, 0x00, 0), // c2.xxxx
        // dsx (p0) r0, c0
        opcode_token(86, 3) | 0x1000_0000, // predicated
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        src_token(19, 0, 0x00, 0), // p0.x
        // dsy_sat_x2 r1, c0  (saturate + mul2)
        opcode_token(87, 2) | (3u32 << 20),
        dst_token(0, 1, 0xF),
        src_token(2, 0, 0xE4, 0),
        // add r2, r0, r1
        opcode_token(2, 3),
        dst_token(0, 2, 0xF),
        src_token(0, 0, 0xE4, 0),
        src_token(0, 1, 0xE4, 0),
        // mov oC0, r2
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 2, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("dpdx("), "{wgsl}");
    assert!(wgsl.contains("dpdy("), "{wgsl}");
    assert!(wgsl.contains("clamp("), "{wgsl}");

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_predicated_derivative_avoids_non_uniform_control_flow() {
    // ps_3_0:
    //   dcl_texcoord0 v0
    //   setp_gt p0, v0.x, c0.x
    //   dsx (p0) r0, v0
    //   mov oC0, r0
    //   end
    //
    // WGSL derivative ops (`dpdx`/`dpdy`) must appear in uniform control flow. A naive predication
    // lowering of `dsx (p0)` as `if (p0) { r0 = dpdx(v0); }` is rejected by naga when `p0` depends
    // on a varying input (here, `v0`).
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_texcoord0 v0  (usage 5 = texcoord)
        opcode_token(31, 1) | (5u32 << 16),
        dst_token(1, 0, 0xF),
        // setp_gt p0, v0.x, c0.x  (compare op 0 = gt)
        opcode_token(78, 3) | (0u32 << 16),
        dst_token(19, 0, 0xF),
        src_token(1, 0, 0x00, 0), // v0.xxxx
        src_token(2, 0, 0x00, 0), // c0.xxxx
        // dsx (p0) r0, v0
        opcode_token(86, 3) | 0x1000_0000, // predicated
        dst_token(0, 0, 0xF),
        src_token(1, 0, 0xE4, 0), // v0
        src_token(19, 0, 0x00, 0), // p0.x
        // mov oC0, r0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("dpdx("), "{wgsl}");
    assert!(wgsl.contains("select("), "{wgsl}");
    assert!(
        !wgsl.contains("if (p0.x)"),
        "predicated derivatives should not lower to an if; got:\n{wgsl}"
    );

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_texkill_is_conditional() {
    // ps_3_0:
    //   texkill r0
    //   mov oC0, c0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // texkill r0
        opcode_token(65, 1),
        src_token(0, 0, 0xE4, 0),
        // mov oC0, c0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;

    // Ensure `texkill` generates the D3D9 rule (discard if any component < 0), not an unconditional
    // discard.
    assert!(wgsl.contains("if (any("), "{wgsl}");
    assert!(wgsl.contains("< vec4<f32>(0.0)"), "{wgsl}");
    assert!(wgsl.contains("discard;"), "{wgsl}");

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_predicated_texkill_is_nested_under_if() {
    // ps_3_0:
    //   texkill (p0) r0
    //   mov oC0, c0
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // texkill (p0) r0
        opcode_token(65, 2) | 0x1000_0000, // predicated
        src_token(0, 0, 0xE4, 0),
        src_token(19, 0, 0x00, 0), // p0.x
        // mov oC0, c0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(wgsl.contains("if (p0.x)"), "{wgsl}");
    let pred_if = wgsl.find("if (p0.x)").expect("predicate if");
    assert!(wgsl[pred_if..].contains("if (any("), "{wgsl}");

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_vs_outputs_and_ps_inputs_use_consistent_locations() {
    // vs_2_0:
    //   dcl_positiont v0
    //   dcl_color0 v7
    //   mov oPos, v0
    //   mov oD0, v7
    //   end
    //
    // ps_2_0:
    //   mov oC0, v0
    //   end
    //
    // The vertex shader should expose oD0 at @location(0), and the pixel shader should read v0
    // from @location(0). The VS also remaps COLOR0 v7 -> @location(6) via StandardLocationMap.
    let vs_tokens = vec![
        version_token(ShaderStage::Vertex, 2, 0),
        // dcl_positiont v0
        31u32 | (2u32 << 24) | (9u32 << 16),
        dst_token(1, 0, 0xF),
        // dcl_color0 v7
        31u32 | (2u32 << 24) | (10u32 << 16),
        dst_token(1, 7, 0xF),
        // mov oPos, v0
        opcode_token(1, 2),
        dst_token(4, 0, 0xF),
        src_token(1, 0, 0xE4, 0),
        // mov oD0, v7
        opcode_token(1, 2),
        dst_token(5, 0, 0xF),
        src_token(1, 7, 0xE4, 0),
        0x0000_FFFF,
    ];
    let ps_tokens = vec![
        version_token(ShaderStage::Pixel, 2, 0),
        // mov oC0, v0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(1, 0, 0xE4, 0),
        0x0000_FFFF,
    ];

    let vs_decoded = decode_u32_tokens(&vs_tokens).unwrap();
    let vs_ir = build_ir(&vs_decoded).unwrap();
    verify_ir(&vs_ir).unwrap();
    let vs_wgsl = generate_wgsl(&vs_ir).unwrap().wgsl;
    assert!(vs_wgsl.contains("@location(6) v6"), "{vs_wgsl}");
    assert!(vs_wgsl.contains("@location(0) oD0"), "{vs_wgsl}");

    let ps_decoded = decode_u32_tokens(&ps_tokens).unwrap();
    let ps_ir = build_ir(&ps_decoded).unwrap();
    verify_ir(&ps_ir).unwrap();
    let ps_wgsl = generate_wgsl(&ps_ir).unwrap().wgsl;
    assert!(ps_wgsl.contains("struct FsIn"), "{ps_wgsl}");
    assert!(ps_wgsl.contains("@location(0) v0"), "{ps_wgsl}");

    // Ensure both shaders are valid WGSL modules.
    let vs_mod = naga::front::wgsl::parse_str(&vs_wgsl).expect("vs wgsl parse");
    let ps_mod = naga::front::wgsl::parse_str(&ps_wgsl).expect("ps wgsl parse");
    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    );
    validator.validate(&vs_mod).expect("vs wgsl validate");
    validator.validate(&ps_mod).expect("ps wgsl validate");
}

#[test]
fn wgsl_missing_dcl_uses_v0_writes_oc0_compiles() {
    // ps_2_0:
    //   mov oC0, v0
    //   end
    //
    // Some real-world SM2 shaders omit `dcl` declarations entirely. The WGSL backend must still
    // declare input/output interface variables based on register usage.
    let tokens = vec![
        version_token(ShaderStage::Pixel, 2, 0),
        // mov oC0, v0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),     // oC0
        src_token(1, 0, 0xE4, 0), // v0
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_ps3_vpos_misctype_builtin_compiles() {
    // ps_3_0:
    //   mov oC0, misc0  (vPos)
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // mov oC0, misc0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(17, 0, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(
        wgsl.contains("@builtin(position) frag_pos: vec4<f32>"),
        "{wgsl}"
    );

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn wgsl_ps3_vface_misctype_builtin_compiles() {
    // ps_3_0:
    //   mov oC0, misc1  (vFace)
    //   end
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // mov oC0, misc1
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(17, 1, 0xE4, 0),
        // end
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;
    assert!(
        wgsl.contains("@builtin(front_facing) front_facing: bool"),
        "{wgsl}"
    );

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}
