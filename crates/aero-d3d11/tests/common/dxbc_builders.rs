use aero_d3d11::sm4::opcode as sm4_opcode;
use aero_dxbc::{test_utils as dxbc_test_utils, FourCC};

fn build_dxbc(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
    dxbc_test_utils::build_container_owned(chunks)
}

fn tokens_to_bytes(tokens: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(tokens.len() * 4);
    for &t in tokens {
        out.extend_from_slice(&t.to_le_bytes());
    }
    out
}

fn opcode_token(opcode: u32, len_dwords: u32) -> u32 {
    opcode | (len_dwords << sm4_opcode::OPCODE_LEN_SHIFT)
}

pub fn build_gs_linelist_to_triangle_dxbc() -> Vec<u8> {
    // gs_4_0:
    //   dcl_inputprimitive line
    //   dcl_outputtopology triangle_strip
    //   dcl_maxvertexcount 3
    //   mov o0, v0[0]; emit
    //   mov o0, v0[1]; emit
    //   mov o0, l(0, 0.5, 0, 1); emit
    //   ret
    //
    // With front_face=CW, the emitted vertices (left->right base + top) form a CCW triangle. Tests
    // can set cull=Front so placeholder prepass output (CW fullscreen triangle) is culled, making
    // assertions sensitive to the translated GS prepass and correct primitive assembly.
    let version_token = 0x0002_0040u32; // gs_4_0
    let mut tokens = vec![version_token, 0];

    tokens.push(opcode_token(sm4_opcode::OPCODE_DCL_GS_INPUT_PRIMITIVE, 2));
    tokens.push(2); // line
    tokens.push(opcode_token(sm4_opcode::OPCODE_DCL_GS_OUTPUT_TOPOLOGY, 2));
    tokens.push(3); // triangle_strip (tokenized shader format)
    tokens.push(opcode_token(
        sm4_opcode::OPCODE_DCL_GS_MAX_OUTPUT_VERTEX_COUNT,
        2,
    ));
    tokens.push(3); // maxvertexcount

    // Minimal I/O decls (opcode value is irrelevant as long as it's treated as a declaration by the decoder).
    const DCL_DUMMY: u32 = 0x300;
    tokens.push(opcode_token(DCL_DUMMY, 3));
    tokens.push(0x0010_F012); // v0.xyzw (1D indexing)
    tokens.push(0);
    tokens.push(opcode_token(DCL_DUMMY + 1, 3));
    tokens.push(0x0010_F022); // o0.xyzw
    tokens.push(0);

    // mov o0.xyzw, v0[0].xyzw
    tokens.push(opcode_token(sm4_opcode::OPCODE_MOV, 6));
    tokens.push(0x0010_F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x0020_F012); // v0[0].xyzw (2D indexing)
    tokens.push(0); // reg
    tokens.push(0); // vertex
    tokens.push(opcode_token(sm4_opcode::OPCODE_EMIT, 1));

    // mov o0.xyzw, v0[1].xyzw
    tokens.push(opcode_token(sm4_opcode::OPCODE_MOV, 6));
    tokens.push(0x0010_F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x0020_F012); // v0[1].xyzw (2D indexing)
    tokens.push(0); // reg
    tokens.push(1); // vertex
    tokens.push(opcode_token(sm4_opcode::OPCODE_EMIT, 1));

    // mov o0.xyzw, l(0, 0.5, 0, 1)
    tokens.push(opcode_token(sm4_opcode::OPCODE_MOV, 8));
    tokens.push(0x0010_F022); // o0.xyzw
    tokens.push(0);
    tokens.push(0x0000_F042); // immediate vec4
    tokens.push(0.0f32.to_bits());
    tokens.push(0.5f32.to_bits());
    tokens.push(0.0f32.to_bits());
    tokens.push(1.0f32.to_bits());
    tokens.push(opcode_token(sm4_opcode::OPCODE_EMIT, 1));

    tokens.push(opcode_token(sm4_opcode::OPCODE_RET, 1));
    tokens[1] = tokens.len() as u32;

    build_dxbc(&[(FourCC(*b"SHDR"), tokens_to_bytes(&tokens))])
}

