mod common;

use aero_d3d11::binding_model::BINDING_BASE_UAV;
use aero_d3d11::runtime::execute::D3D11Runtime;
use aero_d3d11::sm4::decode_program;
use aero_d3d11::sm4::opcode::*;
use aero_d3d11::sm4::{ShaderModel, Sm4Program, FOURCC_SHEX};
use aero_d3d11::{translate_sm4_module_to_wgsl, DxbcFile, ShaderSignatures, Swizzle, WriteMask};
use aero_dxbc::test_utils as dxbc_test_utils;
use aero_gpu::protocol_d3d11::{
    BindingDesc, BindingType, BufferUsage, CmdWriter, PipelineKind, ShaderStageFlags,
};

// Note: translated compute shaders use `@group(2)` (stage-scoped binding model). The
// `protocol_d3d11` runtime (`D3D11Runtime`) binds compute resources at group 2 so translated WGSL
// can execute through the CmdWriter path.

fn opcode_token(opcode: u32, len: u32) -> u32 {
    opcode | (len << OPCODE_LEN_SHIFT)
}

fn make_sm5_program_tokens(stage_type: u16, body_tokens: &[u32]) -> Vec<u32> {
    // Version token layout: type in bits 16.., major in bits 4..7, minor in bits 0..3.
    let version = ((stage_type as u32) << 16) | (5u32 << 4);
    let total_dwords = 2 + body_tokens.len();
    let mut tokens = Vec::with_capacity(total_dwords);
    tokens.push(version);
    tokens.push(total_dwords as u32);
    tokens.extend_from_slice(body_tokens);
    tokens
}

fn tokens_to_bytes(tokens: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(tokens.len() * 4);
    for &t in tokens {
        bytes.extend_from_slice(&t.to_le_bytes());
    }
    bytes
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

fn reg_src(ty: u32, indices: &[u32], swizzle: Swizzle) -> Vec<u32> {
    let token = operand_token(
        ty,
        2,
        OPERAND_SEL_SWIZZLE,
        swizzle_bits(swizzle.0),
        indices.len() as u32,
    );
    let mut out = Vec::new();
    out.push(token);
    out.extend_from_slice(indices);
    out
}

fn imm32_scalar(value: u32) -> Vec<u32> {
    vec![
        operand_token(OPERAND_TYPE_IMMEDIATE32, 1, OPERAND_SEL_SELECT1, 0, 0),
        value,
    ]
}

fn uav_operand(slot: u32, mask: WriteMask) -> Vec<u32> {
    vec![
        operand_token(
            OPERAND_TYPE_UNORDERED_ACCESS_VIEW,
            0,
            OPERAND_SEL_MASK,
            mask.0 as u32,
            1,
        ),
        slot,
    ]
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
fn compute_translate_and_run_compute_builtin_operand_types_from_dxbc() {
    pollster::block_on(async {
        const TEST_NAME: &str = concat!(
            module_path!(),
            "::compute_translate_and_run_compute_builtin_operand_types_from_dxbc"
        );

        let mut rt = match D3D11Runtime::new_for_tests().await {
            Ok(rt) => rt,
            Err(err) => {
                common::skip_or_panic(TEST_NAME, &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };
        if !rt.supports_compute() {
            common::skip_or_panic(TEST_NAME, "compute unsupported");
            return;
        }

        // Use a 2x2 workgroup and dispatch 2x2 workgroups, producing a 4x4 grid (16 invocations).
        //
        // Each invocation writes 4 u32 values (16 bytes) to `u0[global_linear]`:
        //   [global_linear, group_id.x, group_thread_id.x, group_index]
        //
        // This exercises the full DXBC → decode → WGSL → wgpu path for the SM5 operand types:
        // - 32: `SV_DispatchThreadID`
        // - 33: `SV_GroupID`
        // - 34: `SV_GroupThreadID`
        // - 35: `SV_GroupIndex`
        const GROUP_SIZE_X: u32 = 2;
        const GROUP_SIZE_Y: u32 = 2;
        const WORKGROUPS_X: u32 = 2;
        const WORKGROUPS_Y: u32 = 2;
        const GRID_X: u32 = GROUP_SIZE_X * WORKGROUPS_X;
        const GRID_Y: u32 = GROUP_SIZE_Y * WORKGROUPS_Y;
        const INVOCATIONS: u32 = GRID_X * GRID_Y;

        // `global_linear = x | (y << 2)` (GRID_X == 4).
        const LOG2_GRID_X: u32 = 2;
        const INSERT_WIDTH: u32 = 32 - LOG2_GRID_X;

        let size_bytes = (INVOCATIONS as u64) * 16;

        // Build a minimal SM5 compute shader token stream.
        let mut body = Vec::<u32>::new();

        // dcl_thread_group 2, 2, 1
        body.extend_from_slice(&[
            opcode_token(OPCODE_DCL_THREAD_GROUP, 4),
            GROUP_SIZE_X,
            GROUP_SIZE_Y,
            1,
        ]);

        // dcl_uav_raw u0
        let uav0 = uav_operand(0, WriteMask::XYZW);
        body.push(opcode_token(OPCODE_DCL_UAV_RAW, (1 + uav0.len()) as u32));
        body.extend_from_slice(&uav0);

        // bfi r0.x, l(30), l(2), thread_id.yyyy, thread_id.xxxx
        let width = imm32_scalar(INSERT_WIDTH);
        let offset = imm32_scalar(LOG2_GRID_X);
        let insert = reg_src(OPERAND_TYPE_INPUT_THREAD_ID, &[], Swizzle::YYYY);
        let base = reg_src(OPERAND_TYPE_INPUT_THREAD_ID, &[], Swizzle::XXXX);
        let mut bfi = vec![opcode_token(
            OPCODE_BFI,
            (1 + 2 + width.len() + offset.len() + insert.len() + base.len()) as u32,
        )];
        bfi.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 0, WriteMask::X));
        bfi.extend_from_slice(&width);
        bfi.extend_from_slice(&offset);
        bfi.extend_from_slice(&insert);
        bfi.extend_from_slice(&base);
        body.extend_from_slice(&bfi);

        // bfi r1.x, l(28), l(4), r0.xxxx, l(0)
        let width = imm32_scalar(28);
        let offset = imm32_scalar(4);
        let insert = reg_src(OPERAND_TYPE_TEMP, &[0], Swizzle::XXXX);
        let base = imm32_scalar(0);
        let mut bfi = vec![opcode_token(
            OPCODE_BFI,
            (1 + 2 + width.len() + offset.len() + insert.len() + base.len()) as u32,
        )];
        bfi.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 1, WriteMask::X));
        bfi.extend_from_slice(&width);
        bfi.extend_from_slice(&offset);
        bfi.extend_from_slice(&insert);
        bfi.extend_from_slice(&base);
        body.extend_from_slice(&bfi);

        // mov r2.x, r0.x
        let src = reg_src(OPERAND_TYPE_TEMP, &[0], Swizzle::XXXX);
        let mut mov = vec![opcode_token(OPCODE_MOV, (1 + 2 + src.len()) as u32)];
        mov.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 2, WriteMask::X));
        mov.extend_from_slice(&src);
        body.extend_from_slice(&mov);

        // mov r2.y, thread_group_id.x
        let src = reg_src(OPERAND_TYPE_INPUT_THREAD_GROUP_ID, &[], Swizzle::XXXX);
        let mut mov = vec![opcode_token(OPCODE_MOV, (1 + 2 + src.len()) as u32)];
        mov.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 2, WriteMask::Y));
        mov.extend_from_slice(&src);
        body.extend_from_slice(&mov);

        // mov r2.z, thread_id_in_group.x
        let src = reg_src(OPERAND_TYPE_INPUT_THREAD_ID_IN_GROUP, &[], Swizzle::XXXX);
        let mut mov = vec![opcode_token(OPCODE_MOV, (1 + 2 + src.len()) as u32)];
        mov.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 2, WriteMask::Z));
        mov.extend_from_slice(&src);
        body.extend_from_slice(&mov);

        // mov r2.w, thread_id_in_group_flattened (SV_GroupIndex)
        // Encode as a scalar select1 operand.
        let src = vec![operand_token(
            OPERAND_TYPE_INPUT_THREAD_ID_IN_GROUP_FLATTENED,
            1,
            OPERAND_SEL_SELECT1,
            0,
            0,
        )];
        let mut mov = vec![opcode_token(OPCODE_MOV, (1 + 2 + src.len()) as u32)];
        mov.extend_from_slice(&reg_dst(OPERAND_TYPE_TEMP, 2, WriteMask::W));
        mov.extend_from_slice(&src);
        body.extend_from_slice(&mov);

        // store_raw u0.xyzw, r1.x, r2.xyzw
        let uav0 = uav_operand(0, WriteMask::XYZW);
        let addr = reg_src(OPERAND_TYPE_TEMP, &[1], Swizzle::XXXX);
        let val = reg_src(OPERAND_TYPE_TEMP, &[2], Swizzle::XYZW);
        let mut store = vec![opcode_token(
            OPCODE_STORE_RAW,
            (1 + uav0.len() + addr.len() + val.len()) as u32,
        )];
        store.extend_from_slice(&uav0);
        store.extend_from_slice(&addr);
        store.extend_from_slice(&val);
        body.extend_from_slice(&store);

        body.push(opcode_token(OPCODE_RET, 1));

        let shex_tokens = make_sm5_program_tokens(5, &body);
        let shex_bytes = tokens_to_bytes(&shex_tokens);
        let dxbc_bytes = dxbc_test_utils::build_container_owned(&[(FOURCC_SHEX, shex_bytes)]);

        let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
        let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM5 parse");
        assert_eq!(program.stage, aero_d3d11::ShaderStage::Compute);
        assert_eq!(program.model, ShaderModel { major: 5, minor: 0 });

        let module = decode_program(&program).expect("SM5 decode");
        assert_eq!(module.stage, aero_d3d11::ShaderStage::Compute);

        let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &ShaderSignatures::default())
            .expect("compute translation should succeed");
        assert_wgsl_validates(&translated.wgsl);

        for builtin in [
            "@builtin(global_invocation_id)",
            "@builtin(workgroup_id)",
            "@builtin(local_invocation_id)",
            "@builtin(local_invocation_index)",
        ] {
            assert!(
                translated.wgsl.contains(builtin),
                "expected compute builtin {builtin} in WGSL:\n{}",
                translated.wgsl
            );
        }

        let binding_u0 = BINDING_BASE_UAV + 0;
        assert!(
            translated
                .wgsl
                .contains(&format!("@group(2) @binding({binding_u0})")),
            "expected u0 storage buffer binding to use @group(2); wgsl={}",
            translated.wgsl
        );

        const OUT: u32 = 1;
        const READBACK: u32 = 2;
        const SHADER: u32 = 3;
        const PIPELINE: u32 = 4;

        let mut w = CmdWriter::new();
        w.create_buffer(
            OUT,
            size_bytes,
            BufferUsage::STORAGE | BufferUsage::COPY_SRC | BufferUsage::COPY_DST,
        );
        w.create_buffer(
            READBACK,
            size_bytes,
            BufferUsage::MAP_READ | BufferUsage::COPY_DST,
        );
        w.update_buffer(OUT, 0, &vec![0u8; size_bytes as usize]);

        w.create_shader_module_wgsl(SHADER, &translated.wgsl);
        w.create_compute_pipeline(
            PIPELINE,
            SHADER,
            &[BindingDesc {
                binding: binding_u0,
                ty: BindingType::StorageBufferReadWrite,
                visibility: ShaderStageFlags::COMPUTE,
                storage_texture_format: None,
            }],
        );

        w.set_pipeline(PipelineKind::Compute, PIPELINE);
        w.set_bind_buffer(binding_u0, OUT, 0, 0);
        w.begin_compute_pass();
        w.dispatch(WORKGROUPS_X, WORKGROUPS_Y, 1);
        w.end_compute_pass();
        w.copy_buffer_to_buffer(OUT, 0, READBACK, 0, size_bytes);

        rt.execute(&w.finish()).expect("execute command stream");
        rt.poll_wait();
        let data = rt
            .read_buffer(READBACK, 0, size_bytes)
            .await
            .expect("read buffer");

        let mut got = Vec::<u32>::with_capacity((size_bytes / 4) as usize);
        for chunk in data.chunks_exact(4) {
            got.push(u32::from_le_bytes(chunk.try_into().expect("chunk 4 bytes")));
        }
        assert_eq!(got.len() as u32, INVOCATIONS * 4);

        for global_linear in 0..INVOCATIONS {
            let base = (global_linear as usize) * 4;
            assert_eq!(
                got[base],
                global_linear,
                "slot {global_linear} lane0 (global_linear)"
            );

            let global_x = global_linear % GRID_X;
            let global_y = global_linear / GRID_X;
            let expected_group_id_x = global_x / GROUP_SIZE_X;
            let expected_group_thread_id_x = global_x % GROUP_SIZE_X;
            let expected_group_index =
                (global_x % GROUP_SIZE_X) + (global_y % GROUP_SIZE_Y) * GROUP_SIZE_X;

            assert_eq!(
                got[base + 1],
                expected_group_id_x,
                "slot {global_linear} lane1 (group_id.x)"
            );
            assert_eq!(
                got[base + 2],
                expected_group_thread_id_x,
                "slot {global_linear} lane2 (group_thread_id.x)"
            );
            assert_eq!(
                got[base + 3],
                expected_group_index,
                "slot {global_linear} lane3 (group_index)"
            );
        }
    });
}

