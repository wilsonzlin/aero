use aero_d3d9::sm3::types::ShaderStage;
use aero_d3d9::sm3::{build_ir, decode_u32_tokens, generate_wgsl, verify_ir};

fn version_token(stage: ShaderStage, major: u8, minor: u8) -> u32 {
    let prefix = match stage {
        ShaderStage::Vertex => 0xFFFE_0000,
        ShaderStage::Pixel => 0xFFFF_0000,
    };
    prefix | ((major as u32) << 8) | (minor as u32)
}

fn opcode_token(op: u16, length: u8) -> u32 {
    (op as u32) | ((length as u32) << 24)
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

    assert!(wgsl.contains("let b0"));
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

    assert!(wgsl.contains("let i0"));
    assert!(wgsl.contains("loop {"));
}

