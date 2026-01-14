mod common;

use aero_dxbc::test_utils as dxbc_test_utils;
use aero_d3d11::runtime::aerogpu_execute::AerogpuCmdRuntime;
use aero_d3d11::sm4::opcode::*;
use aero_d3d11::FourCC;

const DXBC_GS_EMIT_STREAM1: &[u8] = include_bytes!("fixtures/gs_emit_stream1.dxbc");
const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");

fn build_sm5_gs_emitthen_cut_stream(stream: u32) -> Vec<u8> {
    fn opcode_token(opcode: u32, len: u32) -> u32 {
        opcode | (len << OPCODE_LEN_SHIFT)
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

    fn imm32_scalar(value: u32) -> [u32; 2] {
        [
            operand_token(
                OPERAND_TYPE_IMMEDIATE32,
                1,
                OPERAND_SEL_SELECT1,
                0,
                0,
            ),
            value,
        ]
    }

    fn tokens_to_bytes(tokens: &[u32]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(tokens.len() * 4);
        for &t in tokens {
            bytes.extend_from_slice(&t.to_le_bytes());
        }
        bytes
    }

    // SM5 geometry shader.
    let version = ((2u32) << 16) | (5u32 << 4);
    let stream_op = imm32_scalar(stream);
    let body = [
        opcode_token(OPCODE_EMITTHENCUT_STREAM, 1 + stream_op.len() as u32),
        stream_op[0],
        stream_op[1],
        opcode_token(OPCODE_RET, 1),
    ];
    let declared_len = 2 + body.len() as u32;
    let mut tokens = Vec::with_capacity(declared_len as usize);
    tokens.push(version);
    tokens.push(declared_len);
    tokens.extend_from_slice(&body);

    dxbc_test_utils::build_container_owned(&[(FOURCC_SHEX, tokens_to_bytes(&tokens))])
}

#[test]
fn aerogpu_cmd_runtime_rejects_nonzero_emit_stream_index() {
    pollster::block_on(async {
        let mut rt = match AerogpuCmdRuntime::new_for_tests().await {
            Ok(rt) => rt,
            Err(err) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };

        let emit_then_cut = build_sm5_gs_emitthen_cut_stream(1);
        for (handle, dxbc, op_name) in [
            (1u32, DXBC_GS_EMIT_STREAM1, "emit_stream"),
            (2u32, emit_then_cut.as_slice(), "emitthen_cut_stream"),
        ] {
            let err = rt
                .create_shader_dxbc(handle, dxbc)
                .expect_err("expected create_shader_dxbc to reject non-zero stream index");
            let msg = err.to_string();
            assert!(
                msg.contains(op_name) && msg.contains("stream") && msg.contains("1"),
                "unexpected error: {err:#}"
            );
        }
    });
}
