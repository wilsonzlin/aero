use aero_d3d11::runtime::gs_translate::{
    translate_gs_module_to_wgsl_compute_prepass, GsTranslateError,
};
use aero_d3d11::sm4::decode_program;
use aero_d3d11::sm4::opcode::*;
use aero_d3d11::{
    DstOperand, GsInputPrimitive, GsOutputTopology, OperandModifier, RegFile, RegisterRef,
    ShaderModel, ShaderStage, Sm4Decl, Sm4Inst, Sm4Module, Sm4Program, SrcKind, SrcOperand,
    Swizzle, WriteMask,
};

fn opcode_token(opcode: u32, len_dwords: u32) -> u32 {
    opcode | (len_dwords << OPCODE_LEN_SHIFT)
}

fn operand_token(
    ty: u32,
    num_components: u32,
    selection_mode: u32,
    component_sel: u32,
    index_dim: u32,
) -> u32 {
    let mut token = 0u32;
    token |= num_components & OPERAND_NUM_COMPONENTS_MASK;
    token |= (selection_mode & OPERAND_SELECTION_MODE_MASK) << OPERAND_SELECTION_MODE_SHIFT;
    token |= (ty & OPERAND_TYPE_MASK) << OPERAND_TYPE_SHIFT;
    token |=
        (component_sel & OPERAND_COMPONENT_SELECTION_MASK) << OPERAND_COMPONENT_SELECTION_SHIFT;
    token |= (index_dim & OPERAND_INDEX_DIMENSION_MASK) << OPERAND_INDEX_DIMENSION_SHIFT;
    token |= OPERAND_INDEX_REP_IMMEDIATE32 << OPERAND_INDEX0_REP_SHIFT;
    token |= OPERAND_INDEX_REP_IMMEDIATE32 << OPERAND_INDEX1_REP_SHIFT;
    token |= OPERAND_INDEX_REP_IMMEDIATE32 << OPERAND_INDEX2_REP_SHIFT;
    token
}

fn swizzle_bits(swz: [u8; 4]) -> u32 {
    (swz[0] as u32) | ((swz[1] as u32) << 2) | ((swz[2] as u32) << 4) | ((swz[3] as u32) << 6)
}

fn reg_dst(ty: u32, idx: u32, mask: WriteMask) -> Vec<u32> {
    vec![
        operand_token(ty, 2, OPERAND_SEL_MASK, mask.0 as u32, 1),
        idx,
    ]
}

fn reg_src(ty: u32, idx: u32) -> Vec<u32> {
    vec![
        operand_token(ty, 2, OPERAND_SEL_SWIZZLE, swizzle_bits(Swizzle::XYZW.0), 1),
        idx,
    ]
}

fn reg_src_swizzle_modifier(ty: u32, idx: u32, swz: [u8; 4], modifier: u32) -> Vec<u32> {
    vec![
        operand_token(ty, 2, OPERAND_SEL_SWIZZLE, swizzle_bits(swz), 1) | OPERAND_EXTENDED_BIT,
        modifier << 6,
        idx,
    ]
}

fn imm32_vec4(values: [u32; 4]) -> Vec<u32> {
    let mut out = Vec::with_capacity(1 + 4);
    out.push(operand_token(
        OPERAND_TYPE_IMMEDIATE32,
        2,
        OPERAND_SEL_SWIZZLE,
        swizzle_bits(Swizzle::XYZW.0),
        0,
    ));
    out.extend_from_slice(&values);
    out
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

fn base_gs_tokens() -> Vec<u32> {
    // Nominal gs_4_0 version token (decoder uses program.stage/model, but the header must be
    // well-formed).
    let version_token = 0x0003_0040u32;

    let mut tokens = vec![version_token, 0];

    // Geometry metadata declarations required by `gs_translate`.
    tokens.push(opcode_token(OPCODE_DCL_GS_INPUT_PRIMITIVE, 2));
    tokens.push(3); // D3D10_SB_PRIMITIVE_TRIANGLE
    tokens.push(opcode_token(OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2));
    tokens.push(3); // D3D10_SB_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP
    tokens.push(opcode_token(OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, 2));
    tokens.push(1);

    // Declare outputs so the decoder produces `Sm4Decl::Output` entries (not strictly required by
    // the GS prepass translator, but keeps the token streams realistic).
    // dcl_output o0.xyzw
    tokens.push(opcode_token(0x100, 3));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    // dcl_output o1.xyzw
    tokens.push(opcode_token(0x100, 3));
    tokens.push(0x10F022); // o1.xyzw
    tokens.push(1);

    tokens
}

fn wgsl_from_tokens(mut tokens: Vec<u32>) -> String {
    tokens[1] = tokens.len() as u32;
    let program = Sm4Program {
        stage: ShaderStage::Geometry,
        model: ShaderModel { major: 4, minor: 0 },
        tokens,
    };
    let module = decode_program(&program).expect("decode");
    translate_gs_module_to_wgsl_compute_prepass(&module).expect("translate")
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
    tokens.push(3); // triangle (tokenized shader format)
    tokens.push(opcode_token(OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2));
    tokens.push(3); // triangle_strip (tokenized shader format)
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

#[test]
fn sm4_gs_float_arithmetic_ops_translate_to_wgsl_compute_prepass() {
    // Ensure the GS prepass translator supports a basic set of arithmetic ops that appear in
    // real-world geometry shaders (mul/mad/dp3/dp4/min/max).
    let version_token = 0x0003_0040u32; // nominal gs_4_0 (decoder uses program.stage/model)
    let mut tokens = vec![version_token, 0];

    // Geometry metadata declarations.
    tokens.push(opcode_token(OPCODE_DCL_GS_INPUT_PRIMITIVE, 2));
    tokens.push(3); // triangle (tokenized shader format)
    tokens.push(opcode_token(OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2));
    tokens.push(5); // triangle_strip
    tokens.push(opcode_token(OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, 2));
    tokens.push(1);

    // dcl_output o0.xyzw / o1.xyzw (opcode values are irrelevant; decoder treats opcode>=0x100 as decl).
    const DCL_DUMMY: u32 = 0x100;
    tokens.push(opcode_token(DCL_DUMMY, 3));
    tokens.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    tokens.push(opcode_token(DCL_DUMMY + 1, 3));
    tokens.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 1, WriteMask::XYZW));

    // mov o0.xyzw, l(1, 2, 3, 4)
    let mut mov_o0 = vec![opcode_token(OPCODE_MOV, 0)];
    mov_o0.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    mov_o0.extend_from_slice(&imm32_vec4([
        1.0f32.to_bits(),
        2.0f32.to_bits(),
        3.0f32.to_bits(),
        4.0f32.to_bits(),
    ]));
    mov_o0[0] = opcode_token(OPCODE_MOV, mov_o0.len() as u32);
    tokens.extend_from_slice(&mov_o0);

    // mov o1.xyzw, l(4, 3, 2, 1)
    let mut mov_o1 = vec![opcode_token(OPCODE_MOV, 0)];
    mov_o1.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 1, WriteMask::XYZW));
    mov_o1.extend_from_slice(&imm32_vec4([
        4.0f32.to_bits(),
        3.0f32.to_bits(),
        2.0f32.to_bits(),
        1.0f32.to_bits(),
    ]));
    mov_o1[0] = opcode_token(OPCODE_MOV, mov_o1.len() as u32);
    tokens.extend_from_slice(&mov_o1);

    // mul o0.xyzw, o0.xyzw, l(2, 2, 2, 2)
    let mut mul_o0 = vec![opcode_token(OPCODE_MUL, 0)];
    mul_o0.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    mul_o0.extend_from_slice(&reg_src(OPERAND_TYPE_OUTPUT, 0));
    mul_o0.extend_from_slice(&imm32_vec4([2.0f32.to_bits(); 4]));
    mul_o0[0] = opcode_token(OPCODE_MUL, mul_o0.len() as u32);
    tokens.extend_from_slice(&mul_o0);

    // mad o1.xyzw, o0.xyzw, l(0.5, 0.5, 0.5, 0.5), o1.xyzw
    let mut mad_o1 = vec![opcode_token(OPCODE_MAD, 0)];
    mad_o1.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 1, WriteMask::XYZW));
    mad_o1.extend_from_slice(&reg_src(OPERAND_TYPE_OUTPUT, 0));
    mad_o1.extend_from_slice(&imm32_vec4([0.5f32.to_bits(); 4]));
    mad_o1.extend_from_slice(&reg_src(OPERAND_TYPE_OUTPUT, 1));
    mad_o1[0] = opcode_token(OPCODE_MAD, mad_o1.len() as u32);
    tokens.extend_from_slice(&mad_o1);

    // dp3 o1.xyzw, o0.xyzw, o1.xyzw
    let mut dp3_o1 = vec![opcode_token(OPCODE_DP3, 0)];
    dp3_o1.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 1, WriteMask::XYZW));
    dp3_o1.extend_from_slice(&reg_src(OPERAND_TYPE_OUTPUT, 0));
    dp3_o1.extend_from_slice(&reg_src(OPERAND_TYPE_OUTPUT, 1));
    dp3_o1[0] = opcode_token(OPCODE_DP3, dp3_o1.len() as u32);
    tokens.extend_from_slice(&dp3_o1);

    // dp4 o0.xyzw, o0.xyzw, o1.xyzw
    let mut dp4_o0 = vec![opcode_token(OPCODE_DP4, 0)];
    dp4_o0.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    dp4_o0.extend_from_slice(&reg_src(OPERAND_TYPE_OUTPUT, 0));
    dp4_o0.extend_from_slice(&reg_src(OPERAND_TYPE_OUTPUT, 1));
    dp4_o0[0] = opcode_token(OPCODE_DP4, dp4_o0.len() as u32);
    tokens.extend_from_slice(&dp4_o0);

    // min o0.xyzw, o0.xyzw, l(0, 0, 0, 0)
    let mut min_o0 = vec![opcode_token(OPCODE_MIN, 0)];
    min_o0.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    min_o0.extend_from_slice(&reg_src(OPERAND_TYPE_OUTPUT, 0));
    min_o0.extend_from_slice(&imm32_vec4([0; 4]));
    min_o0[0] = opcode_token(OPCODE_MIN, min_o0.len() as u32);
    tokens.extend_from_slice(&min_o0);

    // max o1.xyzw, o1.xyzw, l(0, 0, 0, 0)
    let mut max_o1 = vec![opcode_token(OPCODE_MAX, 0)];
    max_o1.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 1, WriteMask::XYZW));
    max_o1.extend_from_slice(&reg_src(OPERAND_TYPE_OUTPUT, 1));
    max_o1.extend_from_slice(&imm32_vec4([0; 4]));
    max_o1[0] = opcode_token(OPCODE_MAX, max_o1.len() as u32);
    tokens.extend_from_slice(&max_o1);

    // emit; ret
    tokens.push(opcode_token(OPCODE_EMIT, 1));
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
        wgsl.contains(") * ("),
        "expected mul/mad to translate to a parenthesized multiply expression:\n{wgsl}"
    );
    assert!(
        wgsl.contains("dot(("),
        "expected dp3/dp4 to translate via WGSL dot() intrinsic:\n{wgsl}"
    );
    assert!(
        wgsl.contains("min(("),
        "expected min to translate via WGSL min() intrinsic:\n{wgsl}"
    );
    assert!(
        wgsl.contains("max(("),
        "expected max to translate via WGSL max() intrinsic:\n{wgsl}"
    );

    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_pointlist_output_topology_translates_to_wgsl_compute_prepass() {
    // Minimal gs_4_0 token stream with pointlist output:
    // - dcl_inputprimitive point
    // - dcl_outputtopology pointlist
    // - dcl_maxvertexcount 1
    // - mov o0, v0[0]; emit; ret
    let version_token = 0x0003_0040u32; // nominal gs_4_0 (decoder uses program.stage/model)
    let mut tokens = vec![version_token, 0];

    tokens.push(opcode_token(OPCODE_DCL_GS_INPUT_PRIMITIVE, 2));
    tokens.push(1); // point
    tokens.push(opcode_token(OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2));
    tokens.push(1); // pointlist
    tokens.push(opcode_token(OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, 2));
    tokens.push(1);

    // dcl_input v0.xyzw
    tokens.push(opcode_token(0x100, 3));
    tokens.push(0x10F012); // v0.xyzw (1D indexing)
    tokens.push(0); // v0

    // dcl_output o0.xyzw
    tokens.push(opcode_token(0x100, 3));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);

    // dcl_output o1.xyzw
    tokens.push(opcode_token(0x100, 3));
    tokens.push(0x10F022); // o1.xyzw
    tokens.push(1);

    // mov o0.xyzw, v0[0].xyzw
    tokens.push(opcode_token(OPCODE_MOV, 6));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x20F012); // v0[0].xyzw (2D indexing)
    tokens.push(0); // reg
    tokens.push(0); // vertex

    // emit
    tokens.push(opcode_token(OPCODE_EMIT, 1));

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
        wgsl.contains("// Point list index emission."),
        "expected point list index emission path in WGSL:\n{wgsl}"
    );
    assert!(
        wgsl.contains("out_indices.data[base] = vtx_idx;"),
        "expected point list to emit one index per vertex:\n{wgsl}"
    );

    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_linestrip_output_topology_translates_to_wgsl_compute_prepass() {
    // Minimal gs_4_0 token stream with linestrip output (tokenized-format encoding):
    // - dcl_inputprimitive line
    // - dcl_outputtopology linestrip
    // - dcl_maxvertexcount 4
    // - emit two vertices, cut, emit two vertices, ret
    let version_token = 0x0003_0040u32; // nominal gs_4_0 (decoder uses program.stage/model)
    let mut tokens = vec![version_token, 0];

    tokens.push(opcode_token(OPCODE_DCL_GS_INPUT_PRIMITIVE, 2));
    tokens.push(2); // line
    tokens.push(opcode_token(OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2));
    tokens.push(2); // linestrip (tokenized shader format)
    tokens.push(opcode_token(OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, 2));
    tokens.push(4);

    // dcl_input v0.xyzw
    tokens.push(opcode_token(0x100, 3));
    tokens.push(0x10F012); // v0.xyzw (1D indexing)
    tokens.push(0); // v0

    // dcl_output o0.xyzw
    tokens.push(opcode_token(0x100, 3));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);

    // dcl_output o1.xyzw
    tokens.push(opcode_token(0x100, 3));
    tokens.push(0x10F022); // o1.xyzw
    tokens.push(1);

    // mov o0.xyzw, v0[0].xyzw
    tokens.push(opcode_token(OPCODE_MOV, 6));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x20F012); // v0[0].xyzw (2D indexing)
    tokens.push(0); // reg
    tokens.push(0); // vertex

    // emit
    tokens.push(opcode_token(OPCODE_EMIT, 1));

    // mov o0.xyzw, v0[1].xyzw
    tokens.push(opcode_token(OPCODE_MOV, 6));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x20F012); // v0[1].xyzw (2D indexing)
    tokens.push(0); // reg
    tokens.push(1); // vertex

    // emit
    tokens.push(opcode_token(OPCODE_EMIT, 1));

    // cut
    tokens.push(opcode_token(OPCODE_CUT, 1));

    // mov o0.xyzw, v0[0].xyzw
    tokens.push(opcode_token(OPCODE_MOV, 6));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x20F012); // v0[0].xyzw (2D indexing)
    tokens.push(0); // reg
    tokens.push(0); // vertex

    // emit
    tokens.push(opcode_token(OPCODE_EMIT, 1));

    // mov o0.xyzw, v0[1].xyzw
    tokens.push(opcode_token(OPCODE_MOV, 6));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x20F012); // v0[1].xyzw (2D indexing)
    tokens.push(0); // reg
    tokens.push(1); // vertex

    // emit
    tokens.push(opcode_token(OPCODE_EMIT, 1));

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
        wgsl.contains("// Line strip -> line list index emission."),
        "expected line strip index emission path in WGSL:\n{wgsl}"
    );
    assert!(
        wgsl.contains("out_indices.data[base] = *strip_prev0;"),
        "expected line strip to emit line-list indices:\n{wgsl}"
    );
    assert!(
        wgsl.contains("out_indices.data[base + 1u] = vtx_idx;"),
        "expected line strip to emit pairs of indices:\n{wgsl}"
    );
    assert!(
        wgsl.contains("gs_cut(&strip_len)"),
        "expected cut lowering to reset strip_len:\n{wgsl}"
    );

    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_linestrip_output_topology_d3d_encoding_translates() {
    // Some toolchains encode `dcl_outputtopology` using D3D primitive topology constants.
    // For linestrip that means `3` (D3D10_PRIMITIVE_TOPOLOGY_LINESTRIP).
    //
    // Use a triangle input encoded as `4` (D3D10_PRIMITIVE_TOPOLOGY_TRIANGLELIST) so the translator
    // can infer the encoding style and disambiguate output_topology=3 (line strip vs triangle strip).
    let version_token = 0x0003_0040u32; // nominal gs_4_0 (decoder uses program.stage/model)
    let mut tokens = vec![version_token, 0];

    tokens.push(opcode_token(OPCODE_DCL_GS_INPUT_PRIMITIVE, 2));
    tokens.push(4); // triangle (D3D10_PRIMITIVE_TOPOLOGY_TRIANGLELIST)
    tokens.push(opcode_token(OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2));
    tokens.push(3); // linestrip (D3D10_PRIMITIVE_TOPOLOGY_LINESTRIP)
    tokens.push(opcode_token(OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, 2));
    tokens.push(2);

    // dcl_input v0.xyzw
    tokens.push(opcode_token(0x100, 3));
    tokens.push(0x10F012); // v0.xyzw (1D indexing)
    tokens.push(0); // v0

    // dcl_output o0.xyzw
    tokens.push(opcode_token(0x100, 3));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);

    // dcl_output o1.xyzw
    tokens.push(opcode_token(0x100, 3));
    tokens.push(0x10F022); // o1.xyzw
    tokens.push(1);

    // mov o0.xyzw, v0[0].xyzw
    tokens.push(opcode_token(OPCODE_MOV, 6));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x20F012); // v0[0].xyzw (2D indexing)
    tokens.push(0); // reg
    tokens.push(0); // vertex

    // emit
    tokens.push(opcode_token(OPCODE_EMIT, 1));

    // mov o0.xyzw, v0[1].xyzw
    tokens.push(opcode_token(OPCODE_MOV, 6));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x20F012); // v0[1].xyzw (2D indexing)
    tokens.push(0); // reg
    tokens.push(1); // vertex

    // emit
    tokens.push(opcode_token(OPCODE_EMIT, 1));

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
        wgsl.contains("// Line strip -> line list index emission."),
        "expected d3d-encoded line strip output topology to translate:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_emit_cut_fixture_translates() {
    // The checked-in fixture uses decl encodings that differ from the tokenized-format enums
    // (triangle=4, triangle_strip=5). The GS prepass translator should accept these encodings so it
    // can run real DXBC blobs produced by various toolchains.
    const DXBC: &[u8] = include_bytes!("fixtures/gs_cut.dxbc");

    let program = Sm4Program::parse_from_dxbc_bytes(DXBC).expect("SM4 parse");
    assert_eq!(program.stage, ShaderStage::Geometry);
    assert_eq!(program.model, ShaderModel { major: 4, minor: 0 });

    let module = decode_program(&program).expect("decode");
    let wgsl = translate_gs_module_to_wgsl_compute_prepass(&module).expect("translate");

    assert!(
        wgsl.contains("GS_MAX_VERTEX_COUNT"),
        "expected generated WGSL to include max vertex count constant"
    );
    assert!(
        wgsl.contains("arrayLength(&out_vertices.data)"),
        "expected generated WGSL to bounds-check out_vertices"
    );

    assert_wgsl_validates(&wgsl);
}

#[test]
fn gs_translate_parallelizes_cs_main_and_uses_atomics() {
    const DXBC: &[u8] = include_bytes!("fixtures/gs_emit_cut.dxbc");

    let program = Sm4Program::parse_from_dxbc_bytes(DXBC).expect("SM4 parse");
    let module = decode_program(&program).expect("decode");
    let wgsl = translate_gs_module_to_wgsl_compute_prepass(&module).expect("translate");

    assert!(
        wgsl.contains(
            "fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {\n  let prim_id: u32 = id.x;"
        ),
        "expected cs_main to treat global_invocation_id.x as prim_id (no single-thread guard):\n{wgsl}"
    );
    assert!(
        !wgsl.contains("for (var prim_id: u32 = 0u; prim_id < params.primitive_count"),
        "expected cs_main to process exactly one primitive per invocation (no prim_id loop):\n{wgsl}"
    );
    assert!(
        wgsl.contains("atomicAdd"),
        "expected translated WGSL to use atomic counters for append allocation:\n{wgsl}"
    );

    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm5_gs_emit_stream_cut_stream_fixture_rejects_nonzero_stream() {
    const DXBC: &[u8] = include_bytes!("fixtures/gs_emit_stream_cut_stream.dxbc");

    let program = Sm4Program::parse_from_dxbc_bytes(DXBC).expect("SM4 parse");
    assert_eq!(program.stage, ShaderStage::Geometry);
    assert_eq!(program.model, ShaderModel { major: 5, minor: 0 });

    let module = decode_program(&program).expect("decode");
    let err = translate_gs_module_to_wgsl_compute_prepass(&module)
        .expect_err("expected GS translator to reject non-zero stream indices");
    assert_eq!(
        err,
        GsTranslateError::UnsupportedStream {
            inst_index: 0,
            opcode: "emit",
            stream: 2
        }
    );
}

#[test]
fn sm4_gs_emitthen_cut_translates_to_wgsl_compute_prepass() {
    // Minimal gs_4_0 token stream with `emitthen_cut` on stream 0.
    //
    // - dcl_inputprimitive triangle
    // - dcl_outputtopology triangle_strip
    // - dcl_maxvertexcount 1
    // - mov o0, v0[0]
    // - emitthen_cut
    // - ret
    let version_token = 0x0003_0040u32; // nominal gs_4_0 (decoder uses program.stage/model)
    let mut tokens = vec![version_token, 0];

    tokens.push(opcode_token(OPCODE_DCL_GS_INPUT_PRIMITIVE, 2));
    tokens.push(3); // D3D10_SB_PRIMITIVE_TRIANGLE
    tokens.push(opcode_token(OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2));
    tokens.push(5); // D3D10_SB_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP
    tokens.push(opcode_token(OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, 2));
    tokens.push(1);

    // dcl_input v0.xyzw
    tokens.push(opcode_token(0x100, 3));
    tokens.push(0x10F012); // v0.xyzw (1D indexing)
    tokens.push(0); // v0

    // dcl_output o0.xyzw
    tokens.push(opcode_token(0x100, 3));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);

    // mov o0.xyzw, v0[0].xyzw
    tokens.push(opcode_token(OPCODE_MOV, 6));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x20F012); // v0[0].xyzw (2D indexing)
    tokens.push(0); // reg
    tokens.push(0); // vertex

    // emitthen_cut (stream 0)
    tokens.push(opcode_token(OPCODE_EMITTHENCUT, 1));

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
    assert!(
        wgsl.contains("// emitthen_cut"),
        "expected generated WGSL to tag emitthen_cut lowering"
    );

    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm5_gs_instance_id_translates_to_wgsl_compute_prepass() {
    // D3D name token for `SV_GSInstanceID`.
    const D3D_NAME_GS_INSTANCE_ID: u32 = 11;
    const DCL_DUMMY: u32 = 0x100;

    let version_token = 0x0003_0050u32; // nominal gs_5_0 (decoder uses program.stage/model)
    let mut tokens = vec![version_token, 0];

    // Geometry metadata declarations.
    tokens.push(opcode_token(OPCODE_DCL_GS_INPUT_PRIMITIVE, 2));
    tokens.push(3); // triangle (tokenized shader format)
    tokens.push(opcode_token(OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2));
    // Use the D3D primitive-topology constant here to ensure the translator tolerates both
    // tokenized-format and topology-style encodings.
    tokens.push(5); // triangle_strip
    tokens.push(opcode_token(OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT, 2));
    tokens.push(1);
    tokens.push(opcode_token(OPCODE_DCL_GS_INSTANCE_COUNT, 2));
    tokens.push(2);

    // dcl_input_siv v0.x, SV_GSInstanceID
    tokens.push(opcode_token(DCL_DUMMY, 4));
    tokens.extend_from_slice(&reg_dst(OPERAND_TYPE_INPUT, 0, WriteMask::X));
    tokens.push(D3D_NAME_GS_INSTANCE_ID);

    // dcl_output o0.xyzw
    tokens.push(opcode_token(DCL_DUMMY + 1, 3));
    tokens.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    // dcl_output o1.xyzw
    tokens.push(opcode_token(DCL_DUMMY + 2, 3));
    tokens.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 1, WriteMask::XYZW));

    // mov o1.xyzw, v0.x
    tokens.push(opcode_token(OPCODE_MOV, 5));
    tokens.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 1, WriteMask::XYZW));
    tokens.extend_from_slice(&reg_src(OPERAND_TYPE_INPUT, 0));

    // emit; ret
    tokens.push(opcode_token(OPCODE_EMIT, 1));
    tokens.push(opcode_token(OPCODE_RET, 1));

    tokens[1] = tokens.len() as u32;

    let program = Sm4Program {
        stage: ShaderStage::Geometry,
        model: ShaderModel { major: 5, minor: 0 },
        tokens,
    };

    let module = decode_program(&program).expect("decode");
    let wgsl = translate_gs_module_to_wgsl_compute_prepass(&module).expect("translate");

    assert!(
        wgsl.contains("const GS_INSTANCE_COUNT: u32 = 2u;"),
        "expected GS instance count to be reflected in WGSL constants"
    );
    assert!(
        wgsl.contains("gs_instance_id"),
        "expected generated WGSL to reference gs_instance_id system value"
    );

    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_mul_translates_to_wgsl_compute_prepass() {
    let mut tokens = base_gs_tokens();

    // mul o0.xyzw, l(1,2,3,4), l(5,6,7,8)
    tokens.push(opcode_token(OPCODE_MUL, 13));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x42); // immediate32 vec4
    tokens.push(0x3f800000); // 1.0
    tokens.push(0x40000000); // 2.0
    tokens.push(0x40400000); // 3.0
    tokens.push(0x40800000); // 4.0
    tokens.push(0x42); // immediate32 vec4
    tokens.push(0x40a00000); // 5.0
    tokens.push(0x40c00000); // 6.0
    tokens.push(0x40e00000); // 7.0
    tokens.push(0x41000000); // 8.0

    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert!(
        wgsl.contains(") * ("),
        "expected generated WGSL to contain a mul expression:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_mul_respects_swizzle_modifier_and_saturate() {
    let mut tokens = base_gs_tokens();

    // mov r0.xyzw, l(1, 2, 3, 4)
    let mut mov_r0 = vec![opcode_token(OPCODE_MOV, 0)];
    mov_r0.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::XYZW));
    mov_r0.extend_from_slice(&imm32_vec4([
        1.0f32.to_bits(),
        2.0f32.to_bits(),
        3.0f32.to_bits(),
        4.0f32.to_bits(),
    ]));
    mov_r0[0] = opcode_token(OPCODE_MOV, mov_r0.len() as u32);
    tokens.extend_from_slice(&mov_r0);

    // mov r1.xyzw, l(5, 6, 7, 8)
    let mut mov_r1 = vec![opcode_token(OPCODE_MOV, 0)];
    mov_r1.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 1, WriteMask::XYZW));
    mov_r1.extend_from_slice(&imm32_vec4([
        5.0f32.to_bits(),
        6.0f32.to_bits(),
        7.0f32.to_bits(),
        8.0f32.to_bits(),
    ]));
    mov_r1[0] = opcode_token(OPCODE_MOV, mov_r1.len() as u32);
    tokens.extend_from_slice(&mov_r1);

    // mul_sat o0.xyzw, -r0.wzyx, abs(r1.zyxw)
    let mut mul_o0 = vec![opcode_token(OPCODE_MUL, 0) | OPCODE_EXTENDED_BIT];
    // Extended opcode token (type=0) with saturate bit set (bit 13).
    mul_o0.push(1u32 << 13);
    mul_o0.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    mul_o0.extend_from_slice(&reg_src_swizzle_modifier(
        OPERAND_TYPE_TEMP,
        0,
        [3, 2, 1, 0],
        1,
    ));
    mul_o0.extend_from_slice(&reg_src_swizzle_modifier(
        OPERAND_TYPE_TEMP,
        1,
        [2, 1, 0, 3],
        2,
    ));
    mul_o0[0] = opcode_token(OPCODE_MUL, mul_o0.len() as u32) | OPCODE_EXTENDED_BIT;
    tokens.extend_from_slice(&mul_o0);

    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert!(
        wgsl.contains(".wzyx"),
        "expected src swizzle to be preserved:\n{wgsl}"
    );
    assert!(
        wgsl.contains("abs("),
        "expected abs modifier to be preserved:\n{wgsl}"
    );
    assert!(
        wgsl.contains("clamp(("),
        "expected saturate flag to lower to clamp():\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_mad_translates_to_wgsl_compute_prepass() {
    let mut tokens = base_gs_tokens();

    // mad o0.xyzw, l(1,1,1,1), l(2,2,2,2), l(3,3,3,3)
    tokens.push(opcode_token(OPCODE_MAD, 18));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x42); // imm
    tokens.extend([0x3f800000; 4]); // 1.0
    tokens.push(0x42); // imm
    tokens.extend([0x40000000; 4]); // 2.0
    tokens.push(0x42); // imm
    tokens.extend([0x40400000; 4]); // 3.0

    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert!(
        wgsl.contains(") * (") && wgsl.contains(") + ("),
        "expected generated WGSL to contain a mad expression:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_min_translates_to_wgsl_compute_prepass() {
    let mut tokens = base_gs_tokens();

    // min o0.xyzw, l(1,2,3,4), l(4,3,2,1)
    tokens.push(opcode_token(OPCODE_MIN, 13));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x42); // imm
    tokens.push(0x3f800000); // 1.0
    tokens.push(0x40000000); // 2.0
    tokens.push(0x40400000); // 3.0
    tokens.push(0x40800000); // 4.0
    tokens.push(0x42); // imm
    tokens.push(0x40800000); // 4.0
    tokens.push(0x40400000); // 3.0
    tokens.push(0x40000000); // 2.0
    tokens.push(0x3f800000); // 1.0

    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert!(
        wgsl.contains("min(("),
        "expected generated WGSL to contain a min() call:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_max_translates_to_wgsl_compute_prepass() {
    let mut tokens = base_gs_tokens();

    // max o0.xyzw, l(1,2,3,4), l(4,3,2,1)
    tokens.push(opcode_token(OPCODE_MAX, 13));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x42); // imm
    tokens.push(0x3f800000); // 1.0
    tokens.push(0x40000000); // 2.0
    tokens.push(0x40400000); // 3.0
    tokens.push(0x40800000); // 4.0
    tokens.push(0x42); // imm
    tokens.push(0x40800000); // 4.0
    tokens.push(0x40400000); // 3.0
    tokens.push(0x40000000); // 2.0
    tokens.push(0x3f800000); // 1.0

    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert!(
        wgsl.contains("max(("),
        "expected generated WGSL to contain a max() call:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_dp3_translates_to_wgsl_compute_prepass() {
    let mut tokens = base_gs_tokens();

    // dp3 o0.x, l(1,2,3,4), l(5,6,7,8)
    tokens.push(opcode_token(OPCODE_DP3, 13));
    tokens.push(0x101022); // o0.x (mask mode, component_sel=1)
    tokens.push(0);
    tokens.push(0x42); // imm
    tokens.push(0x3f800000); // 1.0
    tokens.push(0x40000000); // 2.0
    tokens.push(0x40400000); // 3.0
    tokens.push(0x40800000); // 4.0
    tokens.push(0x42); // imm
    tokens.push(0x40a00000); // 5.0
    tokens.push(0x40c00000); // 6.0
    tokens.push(0x40e00000); // 7.0
    tokens.push(0x41000000); // 8.0

    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert!(
        wgsl.contains("dot((") && wgsl.contains(".xyz"),
        "expected generated WGSL to contain a dp3 dot() call:\n{wgsl}"
    );
    assert!(
        wgsl.contains("o0.x ="),
        "expected write-mask to lower to component assignment:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_dp4_translates_to_wgsl_compute_prepass() {
    let mut tokens = base_gs_tokens();

    // dp4 o0.xyzw, l(1,2,3,4), l(5,6,7,8)
    tokens.push(opcode_token(OPCODE_DP4, 13));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x42); // imm
    tokens.push(0x3f800000); // 1.0
    tokens.push(0x40000000); // 2.0
    tokens.push(0x40400000); // 3.0
    tokens.push(0x40800000); // 4.0
    tokens.push(0x42); // imm
    tokens.push(0x40a00000); // 5.0
    tokens.push(0x40c00000); // 6.0
    tokens.push(0x40e00000); // 7.0
    tokens.push(0x41000000); // 8.0

    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert!(
        wgsl.contains("vec4<f32>(dot(("),
        "expected generated WGSL to contain a dp4 dot() call:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_movc_translates_to_wgsl_compute_prepass() {
    let mut tokens = base_gs_tokens();

    // movc o0.xyzw, l(1,0,0,0), l(2,2,2,2), l(3,3,3,3)
    tokens.push(opcode_token(OPCODE_MOVC, 18));
    tokens.push(0x10F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x42); // cond imm
    tokens.push(0x3f800000); // 1.0 (non-zero => true)
    tokens.push(0);
    tokens.push(0);
    tokens.push(0);
    tokens.push(0x42); // a imm
    tokens.extend([0x40000000; 4]); // 2.0
    tokens.push(0x42); // b imm
    tokens.extend([0x40400000; 4]); // 3.0

    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert!(
        wgsl.contains("select(("),
        "expected generated WGSL to contain a select() call for movc:\n{wgsl}"
    );
    assert!(
        wgsl.contains("!= vec4<u32>(0u)"),
        "expected movc condition to be implemented via bitcast/!=0:\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn sm4_gs_movc_respects_saturate() {
    let mut tokens = base_gs_tokens();

    // movc_sat o0.xyzw, l(0,1,0,1), l(2,2,2,2), l(-1,-1,-1,-1)
    //
    // This test is intentionally string-based: it ensures the GS prepass translator:
    // - lowers movc via WGSL `select` with a vector boolean condition
    // - applies the saturate flag via `clamp` *around* the select expression
    let mut inst = vec![opcode_token(OPCODE_MOVC, 0) | OPCODE_EXTENDED_BIT];
    // Extended opcode token (type=0) with saturate bit set (bit 13).
    inst.push(1u32 << 13);
    inst.extend_from_slice(&reg_dst(OPERAND_TYPE_OUTPUT, 0, WriteMask::XYZW));
    inst.extend_from_slice(&imm32_vec4([
        0.0f32.to_bits(),
        1.0f32.to_bits(),
        0.0f32.to_bits(),
        1.0f32.to_bits(),
    ]));
    inst.extend_from_slice(&imm32_vec4([2.0f32.to_bits(); 4]));
    inst.extend_from_slice(&imm32_vec4([(-1.0f32).to_bits(); 4]));
    inst[0] = opcode_token(OPCODE_MOVC, inst.len() as u32) | OPCODE_EXTENDED_BIT;
    tokens.extend_from_slice(&inst);

    tokens.push(opcode_token(OPCODE_RET, 1));

    let wgsl = wgsl_from_tokens(tokens);
    assert!(
        wgsl.contains("select(("),
        "expected generated WGSL to contain a select() call for movc:\n{wgsl}"
    );
    assert!(
        wgsl.contains("!= vec4<u32>(0u)"),
        "expected movc condition to be implemented via bitcast/!=0:\n{wgsl}"
    );
    assert!(
        wgsl.contains("clamp((select(("),
        "expected saturate flag to wrap the movc select() via clamp():\n{wgsl}"
    );
    assert_wgsl_validates(&wgsl);
}

#[test]
fn gs_translate_rejects_regfile_output_depth_source() {
    let module = Sm4Module {
        stage: ShaderStage::Geometry,
        model: ShaderModel { major: 4, minor: 0 },
        decls: vec![
            Sm4Decl::GsInputPrimitive {
                primitive: GsInputPrimitive::Triangle(3),
            },
            Sm4Decl::GsOutputTopology {
                topology: GsOutputTopology::TriangleStrip(3),
            },
            Sm4Decl::GsMaxOutputVertexCount { max: 3 },
        ],
        instructions: vec![
            Sm4Inst::Mov {
                dst: DstOperand {
                    reg: RegisterRef {
                        file: RegFile::Output,
                        index: 0,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                src: SrcOperand {
                    kind: SrcKind::Register(RegisterRef {
                        file: RegFile::OutputDepth,
                        index: 0,
                    }),
                    swizzle: Swizzle::XYZW,
                    modifier: OperandModifier::None,
                },
            },
            Sm4Inst::Ret,
        ],
    };

    let err = translate_gs_module_to_wgsl_compute_prepass(&module)
        .expect_err("expected GS translator to reject RegFile::OutputDepth sources");
    assert_eq!(
        err,
        GsTranslateError::UnsupportedOperand {
            inst_index: 0,
            opcode: "mov",
            msg: "RegFile::OutputDepth is not supported in GS prepass".to_owned()
        }
    );
}

#[test]
fn gs_translate_rejects_regfile_input_without_siv_decl() {
    let module = Sm4Module {
        stage: ShaderStage::Geometry,
        model: ShaderModel { major: 4, minor: 0 },
        decls: vec![
            Sm4Decl::GsInputPrimitive {
                primitive: GsInputPrimitive::Triangle(3),
            },
            Sm4Decl::GsOutputTopology {
                topology: GsOutputTopology::TriangleStrip(3),
            },
            Sm4Decl::GsMaxOutputVertexCount { max: 3 },
        ],
        instructions: vec![
            Sm4Inst::Mov {
                dst: DstOperand {
                    reg: RegisterRef {
                        file: RegFile::Output,
                        index: 0,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                src: SrcOperand {
                    kind: SrcKind::Register(RegisterRef {
                        file: RegFile::Input,
                        index: 0,
                    }),
                    swizzle: Swizzle::XYZW,
                    modifier: OperandModifier::None,
                },
            },
            Sm4Inst::Ret,
        ],
    };

    let err = translate_gs_module_to_wgsl_compute_prepass(&module)
        .expect_err("expected GS translator to reject RegFile::Input without dcl_input_siv");
    assert_eq!(
        err,
        GsTranslateError::UnsupportedOperand {
            inst_index: 0,
            opcode: "mov",
            msg: "unsupported input register v0 (expected v#[]/SrcKind::GsInput or a supported system value via dcl_input_siv)".to_owned()
        }
    );
}

#[test]
fn gs_translate_rejects_regfile_output_depth_destination() {
    let module = Sm4Module {
        stage: ShaderStage::Geometry,
        model: ShaderModel { major: 4, minor: 0 },
        decls: vec![
            Sm4Decl::GsInputPrimitive {
                primitive: GsInputPrimitive::Triangle(3),
            },
            Sm4Decl::GsOutputTopology {
                topology: GsOutputTopology::TriangleStrip(3),
            },
            Sm4Decl::GsMaxOutputVertexCount { max: 3 },
        ],
        instructions: vec![
            Sm4Inst::Mov {
                dst: DstOperand {
                    reg: RegisterRef {
                        file: RegFile::OutputDepth,
                        index: 0,
                    },
                    mask: WriteMask::XYZW,
                    saturate: false,
                },
                src: SrcOperand {
                    kind: SrcKind::ImmediateF32([0; 4]),
                    swizzle: Swizzle::XYZW,
                    modifier: OperandModifier::None,
                },
            },
            Sm4Inst::Ret,
        ],
    };

    let err = translate_gs_module_to_wgsl_compute_prepass(&module)
        .expect_err("expected GS translator to reject RegFile::OutputDepth destinations");
    assert_eq!(
        err,
        GsTranslateError::UnsupportedOperand {
            inst_index: 0,
            opcode: "mov",
            msg: "unsupported destination register file RegFile::OutputDepth".to_owned()
        }
    );
}
