use aero_d3d11::runtime::gs_translate::translate_gs_module_to_wgsl_compute_prepass;
use aero_d3d11::sm4::decode_program;
use aero_d3d11::sm4::opcode::*;
use aero_d3d11::{ShaderModel, ShaderStage, Sm4Program};

fn opcode_token(opcode: u32, len_dwords: u32) -> u32 {
    opcode | (len_dwords << OPCODE_LEN_SHIFT)
}

fn assert_wgsl_validates(wgsl: &str) {
    let module = naga::front::wgsl::parse_str(wgsl).expect("generated WGSL failed to parse");
    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    );
    validator
        .validate(&module)
        .expect("generated WGSL failed to validate");
}

#[test]
fn sm4_gs_emit_cut_translates_to_wgsl_compute_prepass() {
    // Build a minimal gs_4_0 token stream with:
    // - dcl_inputprimitive triangle
    // - dcl_outputtopology triangle_strip
    // - dcl_maxvertexcount 3
    // - mov o0, v0[0]; mov o1, l(1,0,0,1); emit
    // - mov o0, v0[1]; add o0, o0, l(0,0,0,0); emit
    // - mov o0, v0[2]; emit
    // - cut; ret
    let version_token = 0x0003_0040u32; // nominal gs_4_0 (decoder uses program.stage/model)

    let mut tokens = vec![version_token, 0];

    // Geometry metadata declarations.
    tokens.push(opcode_token(OPCODE_DCL_GS_INPUT_PRIMITIVE, 2));
    tokens.push(3); // D3D10_SB_PRIMITIVE_TRIANGLE
    tokens.push(opcode_token(OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2));
    tokens.push(5); // D3D10_SB_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP
    tokens.push(opcode_token(OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, 2));
    tokens.push(3);

    // dcl_input v0.xyzw (opcode value is irrelevant as long as it's treated as a declaration).
    tokens.push(opcode_token(0x100, 3));
    tokens.push(0x10F012); // v0.xyzw (1D indexing)
    tokens.push(0); // v0

    // dcl_output o0.xyzw
    tokens.push(opcode_token(0x100, 3));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);

    // dcl_output o1.xyzw
    tokens.push(opcode_token(0x100, 3));
    tokens.push(0x10F022); // o#.xyzw
    tokens.push(1);

    // mov o0.xyzw, v0[0].xyzw
    tokens.push(opcode_token(OPCODE_MOV, 6));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x20F012); // v0[0].xyzw (2D indexing)
    tokens.push(0); // reg
    tokens.push(0); // vertex

    // mov o1.xyzw, l(1,0,0,1)
    tokens.push(opcode_token(OPCODE_MOV, 8));
    tokens.push(0x10F022); // o1.xyzw
    tokens.push(1);
    tokens.push(0x42); // immediate32 vec4
    tokens.push(0x3f800000); // 1.0
    tokens.push(0);
    tokens.push(0);
    tokens.push(0x3f800000); // 1.0

    // emit
    tokens.push(opcode_token(OPCODE_EMIT, 1));

    // mov o0.xyzw, v0[1].xyzw
    tokens.push(opcode_token(OPCODE_MOV, 6));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x20F012); // v0[1].xyzw
    tokens.push(0); // reg
    tokens.push(1); // vertex

    // add o0.xyzw, o0.xyzw, l(0,0,0,0)
    tokens.push(opcode_token(OPCODE_ADD, 10));
    tokens.push(0x10F022); // o0.xyzw (dst)
    tokens.push(0);
    tokens.push(0x10F022); // o0.xyzw (src0)
    tokens.push(0);
    tokens.push(0x42); // immediate32 vec4
    tokens.push(0);
    tokens.push(0);
    tokens.push(0);
    tokens.push(0);

    // emit
    tokens.push(opcode_token(OPCODE_EMIT, 1));

    // mov o0.xyzw, v0[2].xyzw
    tokens.push(opcode_token(OPCODE_MOV, 6));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x20F012); // v0[2].xyzw
    tokens.push(0); // reg
    tokens.push(2); // vertex

    // emit
    tokens.push(opcode_token(OPCODE_EMIT, 1));

    // cut
    tokens.push(opcode_token(OPCODE_CUT, 1));

    // ret
    tokens.push(opcode_token(OPCODE_RET, 1));

    tokens[1] = tokens.len() as u32;

    let program = Sm4Program {
        stage: ShaderStage::Geometry,
        model: ShaderModel { major: 4, minor: 0 },
        tokens,
    };

    let module = decode_program(&program).expect("decode");
    let wgsl = translate_gs_module_to_wgsl_compute_prepass(&module).expect("translate");

    assert!(
        wgsl.contains("fn gs_emit"),
        "expected generated WGSL to contain gs_emit helper function"
    );
    assert!(
        wgsl.contains("fn gs_cut"),
        "expected generated WGSL to contain gs_cut helper function"
    );
    assert!(
        wgsl.contains("gs_emit(o0, o1"),
        "expected generated WGSL to call gs_emit"
    );
    assert!(
        wgsl.contains("gs_cut(&strip_len)"),
        "expected generated WGSL to call gs_cut"
    );

    assert_wgsl_validates(&wgsl);
}
