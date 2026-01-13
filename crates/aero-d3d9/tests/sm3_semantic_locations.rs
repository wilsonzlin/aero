use aero_d3d9::sm3::ir::RegFile;
use aero_d3d9::sm3::types::ShaderStage;
use aero_d3d9::sm3::{build_ir, decode::decode_u32_tokens};

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
fn sm3_vertex_shader_semantic_locations_remap_to_standard_map() {
    // vs_2_0:
    //   dcl_positiont v0
    //   dcl_color0 v7
    //   mov oPos, v0
    //   mov oD0, v7
    //   end
    //
    // We expect v7 (COLOR0) to be remapped to canonical @location(6) per StandardLocationMap.
    let tokens = vec![
        version_token(ShaderStage::Vertex, 2, 0),
        // dcl_positiont v0
        31u32 | (1u32 << 24) | (9u32 << 16),
        dst_token(1, 0, 0xF),
        // dcl_color0 v7
        31u32 | (1u32 << 24) | (10u32 << 16),
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

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let ir = build_ir(&decoded).unwrap();

    assert!(
        ir.uses_semantic_locations,
        "expected semantic remapping to be enabled"
    );

    // Ensure the input declaration for COLOR0 (originally v7) was remapped to v6.
    let ir_text = ir.to_string();
    assert!(
        ir_text.contains("v6 = Color(0)"),
        "expected IR to contain remapped COLOR0 input at v6, got:\n{ir_text}"
    );

    // The WGSL backend uses input register indices as WGSL @location values, so this remap
    // directly affects the generated interface.
    let mut input_locs: Vec<u32> = ir
        .inputs
        .iter()
        .filter(|d| d.reg.file == RegFile::Input)
        .map(|d| d.reg.index)
        .collect();
    input_locs.sort_unstable();
    input_locs.dedup();
    let mut wgsl_interface = String::from("struct VsInput {\n");
    for loc in input_locs {
        wgsl_interface.push_str(&format!("  @location({loc}) v{loc}: vec4<f32>,\n"));
    }
    wgsl_interface.push_str("}\n");
    assert!(
        wgsl_interface.contains("@location(6)"),
        "expected interface to use @location(6), got:\n{wgsl_interface}"
    );
}

#[test]
fn sm3_vertex_shader_duplicate_semantic_locations_are_an_error() {
    // vs_2_0:
    //   dcl_position v0
    //   dcl_positiont v1
    //   add r0, v0, v1
    //   mov oPos, r0
    //   end
    //
    // StandardLocationMap maps both POSITION0 and POSITIONT0 to location 0, so this should
    // be rejected when both are used.
    let tokens = vec![
        version_token(ShaderStage::Vertex, 2, 0),
        // dcl_position v0
        31u32 | (1u32 << 24) | (0u32 << 16),
        dst_token(1, 0, 0xF),
        // dcl_positiont v1
        31u32 | (1u32 << 24) | (9u32 << 16) | (0u32 << 20),
        dst_token(1, 1, 0xF),
        // add r0, v0, v1
        opcode_token(2, 3),
        dst_token(0, 0, 0xF),
        src_token(1, 0, 0xE4, 0),
        src_token(1, 1, 0xE4, 0),
        // mov oPos, r0
        opcode_token(1, 2),
        dst_token(4, 0, 0xF),
        src_token(0, 0, 0xE4, 0),
        0x0000_FFFF,
    ];

    let decoded = decode_u32_tokens(&tokens).unwrap();
    let err = build_ir(&decoded).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("@location(0)") && msg.contains("v0") && msg.contains("v1"),
        "expected duplicate location error mentioning @location(0), v0 and v1; got: {msg}"
    );
}
