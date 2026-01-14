use aero_d3d9::sm3::decode::decode_u32_tokens;
use aero_d3d9::sm3::types::ShaderStage;
use aero_d3d9::sm3::{build_ir, generate_wgsl, verify_ir};

fn version_token(stage: ShaderStage, major: u8, minor: u8) -> u32 {
    let prefix = match stage {
        ShaderStage::Vertex => 0xFFFE_0000,
        ShaderStage::Pixel => 0xFFFF_0000,
    };
    prefix | ((major as u32) << 8) | (minor as u32)
}

fn opcode_token(op: u16, operand_count: u8) -> u32 {
    // SM2/3 encode the *total* instruction length in tokens (including the opcode token) in
    // bits 24..27.
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
fn lowers_loop_with_defi_to_bounded_wgsl() {
    // ps_3_0:
    //   defi i0, 0, 3, 1, 0
    //   def c0, 0,0,0,0
    //   def c1, 1,0,0,0
    //   def c2, 2,0,0,0
    //   def c3, 3,0,0,0
    //   def c4, 4,0,0,0
    //   mov r0, c0
    //   loop aL, i0
    //     add r0, r0, c1[aL]
    //   endloop
    //   mov oC0, r0
    //   end
    let mut tokens = Vec::new();
    tokens.push(version_token(ShaderStage::Pixel, 3, 0));

    // defi i0, 0, 3, 1, 0
    tokens.push(opcode_token(82, 5));
    tokens.push(dst_token(7, 0, 0xF));
    tokens.extend([0u32, 3u32, 1u32, 0u32]);

    let def_c = |idx: u32, x_bits: u32, out: &mut Vec<u32>| {
        out.push(opcode_token(81, 5));
        out.push(dst_token(2, idx, 0xF));
        out.extend([x_bits, 0u32, 0u32, 0u32]);
    };
    def_c(0, 0x0000_0000, &mut tokens); // 0.0
    def_c(1, 0x3F80_0000, &mut tokens); // 1.0
    def_c(2, 0x4000_0000, &mut tokens); // 2.0
    def_c(3, 0x4040_0000, &mut tokens); // 3.0
    def_c(4, 0x4080_0000, &mut tokens); // 4.0

    // mov r0, c0
    tokens.push(opcode_token(1, 2));
    tokens.push(dst_token(0, 0, 0xF));
    tokens.push(src_token(2, 0, 0xE4, 0));

    // loop aL, i0
    tokens.push(opcode_token(27, 2));
    tokens.push(src_token(15, 0, 0x00, 0)); // aL.x
    tokens.push(src_token(7, 0, 0xE4, 0)); // i0

    // add r0, r0, c1[aL]
    let mut c1_rel = src_token(2, 1, 0xE4, 0);
    c1_rel |= 0x0000_2000; // RELATIVE flag
    tokens.push(opcode_token(2, 4));
    tokens.push(dst_token(0, 0, 0xF));
    tokens.push(src_token(0, 0, 0xE4, 0));
    tokens.push(c1_rel);
    tokens.push(src_token(15, 0, 0x00, 0)); // aL.x

    // endloop
    tokens.push(opcode_token(29, 0));

    // mov oC0, r0
    tokens.push(opcode_token(1, 2));
    tokens.push(dst_token(8, 0, 0xF));
    tokens.push(src_token(0, 0, 0xE4, 0));

    // end
    tokens.push(0x0000_FFFF);

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;

    // Ensure `aL` is initialized from i0.x and used for relative constant addressing.
    assert!(wgsl.contains("aL.x = (i0).x;"), "{wgsl}");
    assert!(
        wgsl.contains("aero_read_const(u32(clamp(i32(1) + (aL.x)"),
        "{wgsl}"
    );

    // Validate WGSL via naga.
    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn loop_is_always_bounded_by_safety_cap() {
    // A loop with a huge trip count should still be bounded by the safety cap in the generated
    // WGSL, even if the shader body has no explicit break.
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // defi i0, 0, 1000000, 1, 0
        opcode_token(82, 5),
        dst_token(7, 0, 0xF),
        0u32,
        1_000_000u32,
        1u32,
        0u32,
        // mov oC0, c0 (no-op body, but we need an output)
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        // loop aL, i0
        opcode_token(27, 2),
        src_token(15, 0, 0x00, 0),
        src_token(7, 0, 0xE4, 0),
        // endloop
        opcode_token(29, 0),
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();
    let wgsl = generate_wgsl(&ir).unwrap().wgsl;

    // The guard ensures the `loop {}` cannot be infinite.
    assert!(wgsl.contains(">= 1024u"), "{wgsl}");
    assert!(wgsl.contains("if (_aero_loop_step"), "{wgsl}");

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn nested_loops_restore_al() {
    // Build a shader with nested loops reusing `aL`. Correct lowering should save/restore `aL`
    // around each `loop` to emulate SM2/3's loop stack semantics.
    //
    // ps_3_0:
    //   defi i0, 0, 1, 1, 0   ; outer: 2 iterations
    //   defi i1, 0, 0, 1, 0   ; inner: 1 iteration
    //   def  c0, 0,0,0,0
    //   def  c1, 1,0,0,0
    //   def  c2, 2,0,0,0
    //   mov r0, c0
    //   loop aL, i0
    //     add r0, r0, c1[aL]
    //     loop aL, i1
    //       add r0, r0, c2[aL]
    //     endloop
    //     add r0, r0, c1[aL]
    //   endloop
    //   mov oC0, r0
    //   end
    let mut tokens = Vec::new();
    tokens.push(version_token(ShaderStage::Pixel, 3, 0));

    // defi i0, 0, 1, 1, 0
    tokens.push(opcode_token(82, 5));
    tokens.push(dst_token(7, 0, 0xF));
    tokens.extend([0u32, 1u32, 1u32, 0u32]);

    // defi i1, 0, 0, 1, 0
    tokens.push(opcode_token(82, 5));
    tokens.push(dst_token(7, 1, 0xF));
    tokens.extend([0u32, 0u32, 1u32, 0u32]);

    let def_c = |idx: u32, x_bits: u32, out: &mut Vec<u32>| {
        out.push(opcode_token(81, 5));
        out.push(dst_token(2, idx, 0xF));
        out.extend([x_bits, 0u32, 0u32, 0u32]);
    };
    def_c(0, 0x0000_0000, &mut tokens); // 0.0
    def_c(1, 0x3F80_0000, &mut tokens); // 1.0
    def_c(2, 0x4000_0000, &mut tokens); // 2.0

    // mov r0, c0
    tokens.push(opcode_token(1, 2));
    tokens.push(dst_token(0, 0, 0xF));
    tokens.push(src_token(2, 0, 0xE4, 0));

    // loop aL, i0
    tokens.push(opcode_token(27, 2));
    tokens.push(src_token(15, 0, 0x00, 0)); // aL.x
    tokens.push(src_token(7, 0, 0xE4, 0)); // i0

    // add r0, r0, c1[aL]
    let mut c1_rel = src_token(2, 1, 0xE4, 0);
    c1_rel |= 0x0000_2000; // RELATIVE flag
    tokens.push(opcode_token(2, 4));
    tokens.push(dst_token(0, 0, 0xF));
    tokens.push(src_token(0, 0, 0xE4, 0));
    tokens.push(c1_rel);
    tokens.push(src_token(15, 0, 0x00, 0)); // aL.x

    // loop aL, i1
    tokens.push(opcode_token(27, 2));
    tokens.push(src_token(15, 0, 0x00, 0)); // aL.x
    tokens.push(src_token(7, 1, 0xE4, 0)); // i1

    // add r0, r0, c2[aL]
    let mut c2_rel = src_token(2, 2, 0xE4, 0);
    c2_rel |= 0x0000_2000; // RELATIVE flag
    tokens.push(opcode_token(2, 4));
    tokens.push(dst_token(0, 0, 0xF));
    tokens.push(src_token(0, 0, 0xE4, 0));
    tokens.push(c2_rel);
    tokens.push(src_token(15, 0, 0x00, 0)); // aL.x

    // endloop (inner)
    tokens.push(opcode_token(29, 0));

    // add r0, r0, c1[aL]
    let mut c1_rel2 = src_token(2, 1, 0xE4, 0);
    c1_rel2 |= 0x0000_2000; // RELATIVE flag
    tokens.push(opcode_token(2, 4));
    tokens.push(dst_token(0, 0, 0xF));
    tokens.push(src_token(0, 0, 0xE4, 0));
    tokens.push(c1_rel2);
    tokens.push(src_token(15, 0, 0x00, 0)); // aL.x

    // endloop (outer)
    tokens.push(opcode_token(29, 0));

    // mov oC0, r0
    tokens.push(opcode_token(1, 2));
    tokens.push(dst_token(8, 0, 0xF));
    tokens.push(src_token(0, 0, 0xE4, 0));

    // end
    tokens.push(0x0000_FFFF);

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();
    let wgsl = generate_wgsl(&ir).unwrap().wgsl;

    // Each loop should save and restore the loop register.
    assert!(
        wgsl.matches("let _aero_saved_loop_reg").count() >= 2,
        "{wgsl}"
    );
    assert!(
        wgsl.matches("aL = _aero_saved_loop_reg_").count() >= 2,
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
fn wgsl_loop_breakc_then_dsx_is_naga_valid() {
    // ps_3_0:
    //   dcl_texcoord0 v0
    //   def c0, 0,0,0,0
    //   defi i0, 0, 1, 1, 0
    //   mov r0, c0
    //   loop aL, i0
    //     breakc_ne v0.x, c0.x
    //     dsx r0, v0
    //   endloop
    //   mov oC0, r0
    //   end
    //
    // Derivatives after a potentially non-uniform `breakc` are a common uniformity hazard. This
    // test exercises the current lowering and asserts the resulting WGSL remains naga-valid.
    let tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_texcoord0 v0  (usage 5 = texcoord)
        opcode_token(31, 1) | (5u32 << 16),
        dst_token(1, 0, 0xF),
        // def c0, 0, 0, 0, 0
        opcode_token(81, 5),
        dst_token(2, 0, 0xF),
        0u32,
        0u32,
        0u32,
        0u32,
        // defi i0, 0, 1, 1, 0
        opcode_token(82, 5),
        dst_token(7, 0, 0xF),
        0u32,
        1u32,
        1u32,
        0u32,
        // mov r0, c0
        opcode_token(1, 2),
        dst_token(0, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        // loop aL, i0
        opcode_token(27, 2),
        src_token(15, 0, 0x00, 0), // aL.x
        src_token(7, 0, 0xE4, 0),  // i0
        // breakc_ne v0.x, c0.x  (compare op 4 = ne)
        opcode_token(45, 2) | (4u32 << 16),
        src_token(1, 0, 0x00, 0), // v0.xxxx
        src_token(2, 0, 0x00, 0), // c0.xxxx
        // dsx r0, v0
        opcode_token(86, 2),
        dst_token(0, 0, 0xF),
        src_token(1, 0, 0xE4, 0),
        // endloop
        opcode_token(29, 0),
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
    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}

#[test]
fn lowers_rep_with_defi_to_bounded_wgsl() {
    // ps_3_0:
    //   defi i0, 3, 0, 0, 0      ; repeat count = 3
    //   def  c0, 0,0,0,0
    //   def  c1, 1,0,0,0
    //   def  c2, 2,0,0,0
    //   def  c3, 3,0,0,0
    //   mov r0, c0
    //   rep i0
    //     add r0, r0, c1[aL]
    //   endrep
    //   mov oC0, r0
    //   end
    let mut tokens = Vec::new();
    tokens.push(version_token(ShaderStage::Pixel, 3, 0));

    // defi i0, 3, 0, 0, 0
    tokens.push(opcode_token(82, 5));
    tokens.push(dst_token(7, 0, 0xF));
    tokens.extend([3u32, 0u32, 0u32, 0u32]);

    let def_c = |idx: u32, x_bits: u32, out: &mut Vec<u32>| {
        out.push(opcode_token(81, 5));
        out.push(dst_token(2, idx, 0xF));
        out.extend([x_bits, 0u32, 0u32, 0u32]);
    };
    def_c(0, 0x0000_0000, &mut tokens); // 0.0
    def_c(1, 0x3F80_0000, &mut tokens); // 1.0
    def_c(2, 0x4000_0000, &mut tokens); // 2.0
    def_c(3, 0x4040_0000, &mut tokens); // 3.0

    // mov r0, c0
    tokens.push(opcode_token(1, 2));
    tokens.push(dst_token(0, 0, 0xF));
    tokens.push(src_token(2, 0, 0xE4, 0));

    // rep i0
    tokens.push(opcode_token(38, 1));
    tokens.push(src_token(7, 0, 0xE4, 0));

    // add r0, r0, c1[aL]
    let mut c1_rel = src_token(2, 1, 0xE4, 0);
    c1_rel |= 0x0000_2000; // RELATIVE flag
    tokens.push(opcode_token(2, 4));
    tokens.push(dst_token(0, 0, 0xF));
    tokens.push(src_token(0, 0, 0xE4, 0));
    tokens.push(c1_rel);
    tokens.push(src_token(15, 0, 0x00, 0)); // aL.x

    // endrep
    tokens.push(opcode_token(39, 0));

    // mov oC0, r0
    tokens.push(opcode_token(1, 2));
    tokens.push(dst_token(8, 0, 0xF));
    tokens.push(src_token(0, 0, 0xE4, 0));

    // end
    tokens.push(0x0000_FFFF);

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();
    verify_ir(&ir).unwrap();

    let wgsl = generate_wgsl(&ir).unwrap().wgsl;

    // Ensure `rep` initializes `aL.x` to 0 and uses i0.x as the repeat count.
    assert!(wgsl.contains("aL.x = 0;"), "{wgsl}");
    assert!(wgsl.contains("_aero_rep_count: i32 = (i0).x;"), "{wgsl}");
    // Ensure `aL` is used for relative constant addressing.
    assert!(
        wgsl.contains("aero_read_const(u32(clamp(i32(1) + (aL.x)"),
        "{wgsl}"
    );
    // Ensure the bounded-loop safety cap exists.
    assert!(wgsl.contains(">= 1024u"), "{wgsl}");

    let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("wgsl validate");
}
