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
fn wgsl_supports_mova_and_relative_constant_indexing() {
    for stage in [ShaderStage::Vertex, ShaderStage::Pixel] {
        // mov r0, c1[a0.x]
        let mut c1_rel = src_token(2, 1, 0xE4, 0);
        c1_rel |= 0x0000_2000; // RELATIVE flag

        let mut tokens = vec![
            version_token(stage, 3, 0),
            // def c0, 1.0, 0.0, 0.0, 0.0  (used to write a0.x = 1)
            opcode_token(81, 5),
            dst_token(2, 0, 0xF),
            0x3F80_0000,
            0x0000_0000,
            0x0000_0000,
            0x0000_0000,
            // def c1, 0.0, 0.0, 0.0, 0.0
            opcode_token(81, 5),
            dst_token(2, 1, 0xF),
            0x0000_0000,
            0x0000_0000,
            0x0000_0000,
            0x0000_0000,
            // def c2, 2.0, 3.0, 4.0, 5.0  (target of relative indexing)
            opcode_token(81, 5),
            dst_token(2, 2, 0xF),
            0x4000_0000, // 2.0
            0x4040_0000, // 3.0
            0x4080_0000, // 4.0
            0x40A0_0000, // 5.0
            // mova a0.x, c0
            opcode_token(46, 2),
            dst_token(3, 0, 0x1), // a0.x (regtype 3)
            src_token(2, 0, 0xE4, 0),
            // mov r0, c1[a0.x]
            opcode_token(1, 3),
            dst_token(0, 0, 0xF),
            c1_rel,
            src_token(3, 0, 0x00, 0), // a0.x (swizzle xxxx)
        ];

        match stage {
            ShaderStage::Vertex => {
                // mov oPos, r0
                tokens.extend([
                    opcode_token(1, 2),
                    dst_token(4, 0, 0xF),
                    src_token(0, 0, 0xE4, 0),
                ]);
            }
            ShaderStage::Pixel => {
                // mov oC0, r0
                tokens.extend([
                    opcode_token(1, 2),
                    dst_token(8, 0, 0xF),
                    src_token(0, 0, 0xE4, 0),
                ]);
            }
        }
        tokens.push(0x0000_FFFF); // end

        let decoded = decode_u32_tokens(&tokens).unwrap();
        let ir = build_ir(&decoded).unwrap();
        verify_ir(&ir).unwrap();

        let wgsl = generate_wgsl(&ir).unwrap();

        // VS and PS constants occupy separate halves of the shared uniform constant array.
        match stage {
            ShaderStage::Vertex => assert!(
                wgsl.wgsl.contains("const CONST_BASE: u32 = 0u;"),
                "{}",
                wgsl.wgsl
            ),
            ShaderStage::Pixel => assert!(
                wgsl.wgsl.contains("const CONST_BASE: u32 = 256u;"),
                "{}",
                wgsl.wgsl
            ),
        }

        let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("wgsl validate");

        // Address register + clamped relative indexing should be present.
        assert!(wgsl.wgsl.contains("var<private> a0: vec4<i32>"));
        assert!(wgsl.wgsl.contains("a0.x"));
        assert!(wgsl.wgsl.contains("clamp(i32(1)"));

        // Embedded `def c2` must override the uniform constant buffer even for relative indexing.
        assert!(
            wgsl.wgsl
                .contains("const c2: vec4<f32> = vec4<f32>(2.0, 3.0, 4.0, 5.0);"),
            "{}",
            wgsl.wgsl
        );
        assert!(wgsl.wgsl.contains("fn aero_read_const"), "{}", wgsl.wgsl);
        assert!(wgsl.wgsl.contains("return c2;"), "{}", wgsl.wgsl);
    }
}

#[test]
fn wgsl_relative_constant_indexing_uses_addr_component_y() {
    for stage in [ShaderStage::Vertex, ShaderStage::Pixel] {
        // mov r0, c1[a0.y]
        let mut c1_rel = src_token(2, 1, 0xE4, 0);
        c1_rel |= 0x0000_2000; // RELATIVE flag

        let mut tokens = vec![
            version_token(stage, 3, 0),
            // def c0, 0.0, 1.0, 0.0, 0.0  (used to write a0.y = 1)
            opcode_token(81, 5),
            dst_token(2, 0, 0xF),
            0x0000_0000,
            0x3F80_0000,
            0x0000_0000,
            0x0000_0000,
            // def c1, 0.0, 0.0, 0.0, 0.0
            opcode_token(81, 5),
            dst_token(2, 1, 0xF),
            0x0000_0000,
            0x0000_0000,
            0x0000_0000,
            0x0000_0000,
            // def c2, 2.0, 3.0, 4.0, 5.0  (target of relative indexing)
            opcode_token(81, 5),
            dst_token(2, 2, 0xF),
            0x4000_0000, // 2.0
            0x4040_0000, // 3.0
            0x4080_0000, // 4.0
            0x40A0_0000, // 5.0
            // mova a0.y, c0
            opcode_token(46, 2),
            dst_token(3, 0, 0x2), // a0.y (regtype 3)
            src_token(2, 0, 0xE4, 0),
            // mov r0, c1[a0.y]
            opcode_token(1, 3),
            dst_token(0, 0, 0xF),
            c1_rel,
            src_token(3, 0, 0x55, 0), // a0.y (swizzle yyyy)
        ];

        match stage {
            ShaderStage::Vertex => {
                // mov oPos, r0
                tokens.extend([
                    opcode_token(1, 2),
                    dst_token(4, 0, 0xF),
                    src_token(0, 0, 0xE4, 0),
                ]);
            }
            ShaderStage::Pixel => {
                // mov oC0, r0
                tokens.extend([
                    opcode_token(1, 2),
                    dst_token(8, 0, 0xF),
                    src_token(0, 0, 0xE4, 0),
                ]);
            }
        }
        tokens.push(0x0000_FFFF); // end

        let decoded = decode_u32_tokens(&tokens).unwrap();
        let ir = build_ir(&decoded).unwrap();
        verify_ir(&ir).unwrap();

        let wgsl = generate_wgsl(&ir).unwrap();

        let module = naga::front::wgsl::parse_str(&wgsl.wgsl).expect("wgsl parse");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("wgsl validate");

        // Address register + clamped relative indexing should reference the requested component
        // (`a0.y`), not assume `.x`.
        assert!(
            wgsl.wgsl.contains("var<private> a0: vec4<i32>"),
            "{}",
            wgsl.wgsl
        );
        assert!(wgsl.wgsl.contains("a0.y"), "{}", wgsl.wgsl);
        assert!(wgsl.wgsl.contains("clamp(i32(1) + (a0.y)"), "{}", wgsl.wgsl);
        assert!(
            !wgsl.wgsl.contains("clamp(i32(1) + (a0.x)"),
            "{}",
            wgsl.wgsl
        );
    }
}

#[test]
fn wgsl_mova_applies_shift_before_saturate() {
    // `mova` shares result modifier encoding with other float ops. D3D9 defines the ordering as:
    //   shift -> saturate -> float->int conversion.
    //
    // This test ensures we don't accidentally apply saturate before shift in the WGSL lowering.
    for stage in [ShaderStage::Vertex, ShaderStage::Pixel] {
        let mut tokens = vec![
            version_token(stage, 3, 0),
            // mova_sat_div2 a0.x, c0
            //
            // Result modifier bits are stored in opcode_token[20..24], encoded as:
            //   mod_bits = saturate_bit | (shift_bits << 1)
            // For saturate + div2: saturate=1, shift_bits=4 => mod_bits=9.
            opcode_token(46, 2) | (9u32 << 20),
            dst_token(3, 0, 0x1), // a0.x (regtype 3)
            src_token(2, 0, 0xE4, 0),
        ];

        match stage {
            ShaderStage::Vertex => {
                // mov oPos, c0 (dummy output)
                tokens.extend([
                    opcode_token(1, 2),
                    dst_token(4, 0, 0xF),
                    src_token(2, 0, 0xE4, 0),
                ]);
            }
            ShaderStage::Pixel => {
                // mov oC0, c0 (dummy output)
                tokens.extend([
                    opcode_token(1, 2),
                    dst_token(8, 0, 0xF),
                    src_token(2, 0, 0xE4, 0),
                ]);
            }
        }
        tokens.push(0x0000_FFFF); // end

        let decoded = decode_u32_tokens(&tokens).unwrap();
        let ir = build_ir(&decoded).unwrap();
        verify_ir(&ir).unwrap();

        let wgsl = generate_wgsl(&ir).unwrap();
        let src = &wgsl.wgsl;

        // Expect: `clamp((...) / 2.0, 0..1)` (shift then saturate).
        assert!(
            src.contains("/ 2.0, vec4<f32>(0.0), vec4<f32>(1.0))"),
            "{src}"
        );

        // Reject: `clamp(..., 0..1) / 2.0` (saturate then shift).
        assert!(
            !src.contains("vec4<f32>(0.0), vec4<f32>(1.0)) / 2.0"),
            "{src}"
        );

        let module = naga::front::wgsl::parse_str(src).expect("wgsl parse");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("wgsl validate");
    }
}

#[test]
fn wgsl_relative_constant_indexing_without_defs_reads_uniform_buffer_directly() {
    // When there are *no* embedded `def c#` constants, relative constant indexing should read
    // directly from the uniform constant buffer (no `aero_read_const` helper needed).
    for stage in [ShaderStage::Vertex, ShaderStage::Pixel] {
        // mov ???, c1[a0.x]
        let mut c1_rel = src_token(2, 1, 0xE4, 0);
        c1_rel |= 0x0000_2000; // RELATIVE flag

        let mut tokens = vec![
            version_token(stage, 3, 0),
            // mova a0.x, c0
            opcode_token(46, 2),
            dst_token(3, 0, 0x1),     // a0.x (regtype 3)
            src_token(2, 0, 0xE4, 0), // c0
        ];

        match stage {
            ShaderStage::Vertex => {
                // mov oPos, c1[a0.x]
                tokens.extend([
                    opcode_token(1, 3),
                    dst_token(4, 0, 0xF), // oPos
                    c1_rel,
                    src_token(3, 0, 0x00, 0), // a0.x (swizzle xxxx)
                ]);
            }
            ShaderStage::Pixel => {
                // mov oC0, c1[a0.x]
                tokens.extend([
                    opcode_token(1, 3),
                    dst_token(8, 0, 0xF), // oC0
                    c1_rel,
                    src_token(3, 0, 0x00, 0), // a0.x (swizzle xxxx)
                ]);
            }
        }

        tokens.push(0x0000_FFFF); // end

        let decoded = decode_u32_tokens(&tokens).unwrap();
        let ir = build_ir(&decoded).unwrap();
        verify_ir(&ir).unwrap();

        let wgsl = generate_wgsl(&ir).unwrap().wgsl;
        assert!(
            !wgsl.contains("fn aero_read_const"),
            "unexpected aero_read_const helper in WGSL:\n{wgsl}"
        );
        assert!(
            wgsl.contains("constants.c[CONST_BASE + u32(clamp(i32(1) + (a0.x), 0, 255))]"),
            "expected uniform-buffer constant lookup in WGSL:\n{wgsl}"
        );

        let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("wgsl validate");
    }
}

#[test]
fn wgsl_mova_write_mask_preserves_other_components() {
    // `mova` supports write masks (e.g. `mova a0.y, ...`). The WGSL lowering must respect these so
    // a partial write does not clobber other components of the address register.
    for stage in [ShaderStage::Vertex, ShaderStage::Pixel] {
        let mut tokens = vec![
            version_token(stage, 3, 0),
            // def c0, 0.0, 1.0, 0.0, 1.0
            opcode_token(81, 5),
            dst_token(2, 0, 0xF),
            0x0000_0000,
            0x3F80_0000,
            0x0000_0000,
            0x3F80_0000,
            // mova a0.x, c0.x
            opcode_token(46, 2),
            dst_token(3, 0, 0x1),
            src_token(2, 0, 0x00, 0), // c0.xxxx
            // mova a0.y, c0.y
            opcode_token(46, 2),
            dst_token(3, 0, 0x2),
            src_token(2, 0, 0x55, 0), // c0.yyyy
        ];

        match stage {
            ShaderStage::Vertex => {
                // mov oPos, c0
                tokens.extend([
                    opcode_token(1, 2),
                    dst_token(4, 0, 0xF),
                    src_token(2, 0, 0xE4, 0),
                ]);
            }
            ShaderStage::Pixel => {
                // mov oC0, c0
                tokens.extend([
                    opcode_token(1, 2),
                    dst_token(8, 0, 0xF),
                    src_token(2, 0, 0xE4, 0),
                ]);
            }
        }
        tokens.push(0x0000_FFFF);

        let decoded = decode_u32_tokens(&tokens).unwrap();
        let ir = build_ir(&decoded).unwrap();
        verify_ir(&ir).unwrap();
        let wgsl = generate_wgsl(&ir).unwrap().wgsl;

        // Ensure both assignments are per-component (not `a0 = ...`).
        assert!(
            wgsl.contains("a0.x = (vec4<i32>(c0.xxxx)).x;"),
            "{wgsl}"
        );
        assert!(
            wgsl.contains("a0.y = (vec4<i32>(c0.yyyy)).y;"),
            "{wgsl}"
        );
        assert!(!wgsl.contains("a0 = vec4<i32>("), "{wgsl}");

        let module = naga::front::wgsl::parse_str(&wgsl).expect("wgsl parse");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("wgsl validate");
    }
}
