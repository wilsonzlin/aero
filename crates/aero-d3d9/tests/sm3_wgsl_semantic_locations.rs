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

fn struct_member_location(module: &naga::Module, struct_name: &str, member_name: &str) -> u32 {
    for (_, ty) in module.types.iter() {
        if ty.name.as_deref() != Some(struct_name) {
            continue;
        }
        let naga::TypeInner::Struct { members, .. } = &ty.inner else {
            continue;
        };
        for m in members {
            if m.name.as_deref() != Some(member_name) {
                continue;
            }
            if let Some(naga::Binding::Location { location, .. }) = m.binding {
                return location;
            }
        }
    }
    panic!("missing {struct_name}.{member_name} location binding in naga module");
}

#[test]
fn sm3_vs_output_and_ps_input_semantics_share_locations() {
    // Vertex shader:
    //   dcl_position oPos
    //   dcl_texcoord0 o0
    //   def c0, 0, 0, 0, 1
    //   def c1, 0.25, 0.5, 0.75, 1
    //   mov oPos, c0
    //   mov o0, c1
    //   end
    let vs_tokens = vec![
        version_token(ShaderStage::Vertex, 3, 0),
        // dcl_position oPos
        31u32 | (2u32 << 24),
        dst_token(4, 0, 0xF),
        // dcl_texcoord0 o0
        31u32 | (2u32 << 24) | (5u32 << 16),
        dst_token(6, 0, 0xF),
        // def c0, 0, 0, 0, 1
        opcode_token(81, 5),
        dst_token(2, 0, 0xF),
        0x0000_0000,
        0x0000_0000,
        0x0000_0000,
        0x3F80_0000,
        // def c1, 0.25, 0.5, 0.75, 1
        opcode_token(81, 5),
        dst_token(2, 1, 0xF),
        0x3E80_0000,
        0x3F00_0000,
        0x3F40_0000,
        0x3F80_0000,
        // mov oPos, c0
        opcode_token(1, 2),
        dst_token(4, 0, 0xF),
        src_token(2, 0, 0xE4, 0),
        // mov o0, c1
        opcode_token(1, 2),
        dst_token(6, 0, 0xF),
        src_token(2, 1, 0xE4, 0),
        0x0000_FFFF,
    ];

    // Pixel shader:
    //   dcl_texcoord0 v0
    //   mov oC0, v0
    //   end
    let ps_tokens = vec![
        version_token(ShaderStage::Pixel, 3, 0),
        // dcl_texcoord0 v0
        31u32 | (2u32 << 24) | (5u32 << 16),
        dst_token(1, 0, 0xF),
        // mov oC0, v0
        opcode_token(1, 2),
        dst_token(8, 0, 0xF),
        src_token(1, 0, 0xE4, 0),
        0x0000_FFFF,
    ];

    let vs_decoded = decode_u32_tokens(&vs_tokens).unwrap();
    let vs_ir = build_ir(&vs_decoded).unwrap();
    verify_ir(&vs_ir).unwrap();
    let vs_wgsl = generate_wgsl(&vs_ir).unwrap();

    let ps_decoded = decode_u32_tokens(&ps_tokens).unwrap();
    let ps_ir = build_ir(&ps_decoded).unwrap();
    verify_ir(&ps_ir).unwrap();
    let ps_wgsl = generate_wgsl(&ps_ir).unwrap();

    let vs_module = naga::front::wgsl::parse_str(&vs_wgsl.wgsl).expect("vs wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&vs_module)
    .expect("vs wgsl validate");

    let ps_module = naga::front::wgsl::parse_str(&ps_wgsl.wgsl).expect("ps wgsl parse");
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&ps_module)
    .expect("ps wgsl validate");

    let vs_loc = struct_member_location(&vs_module, "VsOut", "o0");
    let ps_loc = struct_member_location(&ps_module, "FsIn", "v0");
    assert_eq!(vs_loc, ps_loc, "VS and PS varyings must share locations");
    assert_eq!(vs_loc, 4, "TEXCOORD0 should map to legacy location 4");
}
