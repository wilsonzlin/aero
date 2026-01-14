mod common;

use aero_d3d11::binding_model::BINDING_BASE_UAV;
use aero_d3d11::runtime::execute::D3D11Runtime;
use aero_d3d11::sm4_ir::ComputeBuiltin;
use aero_d3d11::{
    translate_sm4_module_to_wgsl, BufferKind, DstOperand, DxbcFile, OperandModifier, RegFile,
    RegisterRef, ShaderModel, ShaderSignatures, ShaderStage, Sm4Decl, Sm4Inst, Sm4Module, SrcKind,
    SrcOperand, Swizzle, UavRef, WriteMask,
};
use aero_dxbc::test_utils as dxbc_test_utils;
use aero_gpu::protocol_d3d11::{
    BindingDesc, BindingType, BufferUsage, CmdWriter, PipelineKind, ShaderStageFlags,
};

// Note: translated compute shaders use `@group(2)` (stage-scoped binding model). The
// `protocol_d3d11` runtime (`D3D11Runtime`) binds compute resources at group 2 so translated WGSL
// can execute through the CmdWriter path.

fn dummy_dxbc_bytes() -> Vec<u8> {
    // Minimal DXBC container with no chunks. The signature-driven SM4→WGSL translator only uses the
    // DXBC input for diagnostics, so this is sufficient for compute-stage tests.
    dxbc_test_utils::build_container(&[])
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

fn dst(file: RegFile, index: u32, mask: WriteMask) -> DstOperand {
    DstOperand {
        reg: RegisterRef { file, index },
        mask,
        saturate: false,
    }
}

fn src_reg(file: RegFile, index: u32, swizzle: Swizzle) -> SrcOperand {
    SrcOperand {
        kind: SrcKind::Register(RegisterRef { file, index }),
        swizzle,
        modifier: OperandModifier::None,
    }
}

fn src_imm_u32(value: u32) -> SrcOperand {
    SrcOperand {
        kind: SrcKind::ImmediateF32([value, 0, 0, 0]),
        swizzle: Swizzle::XYZW,
        modifier: OperandModifier::None,
    }
}

fn src_compute_builtin(builtin: ComputeBuiltin, swizzle: Swizzle) -> SrcOperand {
    SrcOperand {
        kind: SrcKind::ComputeBuiltin(builtin),
        swizzle,
        modifier: OperandModifier::None,
    }
}

#[test]
fn compute_translate_and_run_dispatch_thread_id_writes_indexed_uav_buffer() {
    pollster::block_on(async {
        const TEST_NAME: &str = concat!(
            module_path!(),
            "::compute_translate_and_run_dispatch_thread_id_writes_indexed_uav_buffer"
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

        // Use a workgroup size > 1 to ensure `SV_DispatchThreadID` is lowered to
        // `@builtin(global_invocation_id)` (and not accidentally to `workgroup_id`).
        const GROUP_SIZE_X: u32 = 2;
        const GROUP_SIZE_Y: u32 = 2;
        const WORKGROUPS_X: u32 = 2;
        const WORKGROUPS_Y: u32 = 2;
        const GRID_X: u32 = GROUP_SIZE_X * WORKGROUPS_X;
        const GRID_Y: u32 = GROUP_SIZE_Y * WORKGROUPS_Y;
        // GRID_X == 4, so `log2(GRID_X) == 2`.
        const LOG2_GRID_X: u32 = 2;
        const INSERT_WIDTH: u32 = 32 - LOG2_GRID_X;
        const ELEMENTS: u32 = GRID_X * GRID_Y;
        let size_bytes = (ELEMENTS as u64) * 4;

        // D3D10_SB_NAME_DISPATCH_THREAD_ID.
        const D3D_NAME_DISPATCH_THREAD_ID: u32 = 20;

        // Build an SM4 IR module that flattens `SV_DispatchThreadID.xy` into a linear index and
        // writes that index into a UAV buffer at `u0[index]`.
        //
        // Since `store_raw` takes a byte offset, compute `addr = linear << 2` using the `bfi`
        // (bitfield insert) instruction:
        //   addr = insertBits(0, linear, 2, 30)
        //
        // This test is specifically sensitive to whether the builtin is expanded into the untyped
        // `vec4<f32>` register model as *raw integer bits* (via bitcast) rather than float numeric
        // values; numeric conversion would produce float bit patterns and cause out-of-bounds UAV
        // writes.
        let module = Sm4Module {
            stage: ShaderStage::Compute,
            model: ShaderModel { major: 5, minor: 0 },
            decls: vec![
                Sm4Decl::ThreadGroupSize {
                    x: GROUP_SIZE_X,
                    y: GROUP_SIZE_Y,
                    z: 1,
                },
                Sm4Decl::InputSiv {
                    reg: 0,
                    mask: WriteMask::XYZW,
                    sys_value: D3D_NAME_DISPATCH_THREAD_ID,
                },
                Sm4Decl::UavBuffer {
                    slot: 0,
                    stride: 0,
                    kind: BufferKind::Raw,
                },
            ],
            instructions: vec![
                // r0.x = linear = id.x | (id.y << log2(GRID_X))
                Sm4Inst::Bfi {
                    dst: dst(RegFile::Temp, 0, WriteMask::X),
                    width: src_imm_u32(INSERT_WIDTH),
                    offset: src_imm_u32(LOG2_GRID_X),
                    insert: src_reg(RegFile::Input, 0, Swizzle::YYYY),
                    base: src_reg(RegFile::Input, 0, Swizzle::XXXX),
                },
                // r1.x = addr = linear << 2 (byte address)
                Sm4Inst::Bfi {
                    dst: dst(RegFile::Temp, 1, WriteMask::X),
                    width: src_imm_u32(30),
                    offset: src_imm_u32(2),
                    insert: src_reg(RegFile::Temp, 0, Swizzle::XXXX),
                    base: src_imm_u32(0),
                },
                // store_raw u0.x, r1.x, r0.x
                Sm4Inst::StoreRaw {
                    uav: UavRef { slot: 0 },
                    addr: src_reg(RegFile::Temp, 1, Swizzle::XXXX),
                    value: src_reg(RegFile::Temp, 0, Swizzle::XXXX),
                    mask: WriteMask::X,
                },
                Sm4Inst::Ret,
            ],
        };

        let dxbc_bytes = dummy_dxbc_bytes();
        let dxbc = DxbcFile::parse(&dxbc_bytes).expect("dummy DXBC should parse");
        let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &ShaderSignatures::default())
            .expect("compute translation should succeed");
        assert_wgsl_validates(&translated.wgsl);
        assert!(
            translated.wgsl.contains("@builtin(global_invocation_id)"),
            "expected compute builtin in WGSL:\n{}",
            translated.wgsl
        );
        let binding_u0 = BINDING_BASE_UAV;
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
        let mut got = Vec::<u32>::with_capacity(ELEMENTS as usize);
        for i in 0..ELEMENTS as usize {
            let at = i * 4;
            got.push(u32::from_le_bytes(
                data[at..at + 4].try_into().expect("read 4 bytes"),
            ));
        }
        let expected: Vec<u32> = (0..ELEMENTS).collect();
        assert_eq!(got, expected);
    });
}

#[test]
fn compute_translate_and_run_group_id_and_group_index_write_linear_id() {
    pollster::block_on(async {
        const TEST_NAME: &str = concat!(
            module_path!(),
            "::compute_translate_and_run_group_id_and_group_index_write_linear_id"
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

        // D3D10_SB_NAME_GROUP_ID / D3D10_SB_NAME_GROUP_INDEX.
        const D3D_NAME_GROUP_ID: u32 = 21;
        const D3D_NAME_GROUP_INDEX: u32 = 22;

        // Use a 2x2 workgroup (4 threads) and dispatch 4 workgroups: total invocations = 16.
        //
        // Compute `global = (group_id.x << 2) | group_index` using `bfi` (no integer add opcode in
        // our minimal IR) and write it to `u0[global]`.
        const ELEMENTS: u32 = 16;
        let size_bytes = (ELEMENTS as u64) * 4;

        let module = Sm4Module {
            stage: ShaderStage::Compute,
            model: ShaderModel { major: 5, minor: 0 },
            decls: vec![
                Sm4Decl::ThreadGroupSize { x: 2, y: 2, z: 1 },
                Sm4Decl::InputSiv {
                    reg: 0,
                    mask: WriteMask::XYZW,
                    sys_value: D3D_NAME_GROUP_ID,
                },
                Sm4Decl::InputSiv {
                    reg: 1,
                    mask: WriteMask::XYZW,
                    sys_value: D3D_NAME_GROUP_INDEX,
                },
                Sm4Decl::UavBuffer {
                    slot: 0,
                    stride: 0,
                    kind: BufferKind::Raw,
                },
            ],
            instructions: vec![
                // r0.x = (group_id.x << 2) | group_index
                Sm4Inst::Bfi {
                    dst: dst(RegFile::Temp, 0, WriteMask::X),
                    width: src_imm_u32(30),
                    offset: src_imm_u32(2),
                    insert: src_reg(RegFile::Input, 0, Swizzle::XXXX),
                    base: src_reg(RegFile::Input, 1, Swizzle::XXXX),
                },
                // r1.x = global << 2 (byte address)
                Sm4Inst::Bfi {
                    dst: dst(RegFile::Temp, 1, WriteMask::X),
                    width: src_imm_u32(30),
                    offset: src_imm_u32(2),
                    insert: src_reg(RegFile::Temp, 0, Swizzle::XXXX),
                    base: src_imm_u32(0),
                },
                // store_raw u0.x, r1.x, r0.x
                Sm4Inst::StoreRaw {
                    uav: UavRef { slot: 0 },
                    addr: src_reg(RegFile::Temp, 1, Swizzle::XXXX),
                    value: src_reg(RegFile::Temp, 0, Swizzle::XXXX),
                    mask: WriteMask::X,
                },
                Sm4Inst::Ret,
            ],
        };

        let dxbc_bytes = dummy_dxbc_bytes();
        let dxbc = DxbcFile::parse(&dxbc_bytes).expect("dummy DXBC should parse");
        let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &ShaderSignatures::default())
            .expect("compute translation should succeed");
        assert_wgsl_validates(&translated.wgsl);
        assert!(
            translated.wgsl.contains("@builtin(workgroup_id)"),
            "expected workgroup_id builtin in WGSL:\n{}",
            translated.wgsl
        );
        assert!(
            translated.wgsl.contains("@builtin(local_invocation_index)"),
            "expected local_invocation_index builtin in WGSL:\n{}",
            translated.wgsl
        );
        let binding_u0 = BINDING_BASE_UAV;
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
        // 4 workgroups, each of size 4 => 16 invocations total.
        w.dispatch(4, 1, 1);
        w.end_compute_pass();
        w.copy_buffer_to_buffer(OUT, 0, READBACK, 0, size_bytes);

        rt.execute(&w.finish()).expect("execute command stream");
        rt.poll_wait();
        let data = rt
            .read_buffer(READBACK, 0, size_bytes)
            .await
            .expect("read buffer");

        let mut got = Vec::<u32>::with_capacity(ELEMENTS as usize);
        for i in 0..ELEMENTS as usize {
            let at = i * 4;
            got.push(u32::from_le_bytes(
                data[at..at + 4].try_into().expect("read 4 bytes"),
            ));
        }
        let expected: Vec<u32> = (0..ELEMENTS).collect();
        assert_eq!(got, expected);
    });
}

#[test]
fn compute_translate_and_run_group_thread_id_writes_linear_local_index() {
    pollster::block_on(async {
        const TEST_NAME: &str = concat!(
            module_path!(),
            "::compute_translate_and_run_group_thread_id_writes_linear_local_index"
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

        // D3D10_SB_NAME_GROUP_THREAD_ID / D3D10_SB_NAME_GROUP_ID / D3D10_SB_NAME_GROUP_INDEX.
        const D3D_NAME_GROUP_THREAD_ID: u32 = 23;
        const D3D_NAME_GROUP_ID: u32 = 21;
        const D3D_NAME_GROUP_INDEX: u32 = 22;

        // Two 2x2 workgroups = 8 invocations.
        const WORKGROUPS_X: u32 = 2;
        const ELEMENTS: u32 = 8;
        let size_bytes = (ELEMENTS as u64) * 4;

        // Compute `linear = id.x | (id.y << 1)` via `bfi` (expected to be `0..3` for each
        // workgroup), then write it to a unique global slot computed from `SV_GroupID` and
        // `SV_GroupIndex`.
        //
        // This is sensitive to whether `SV_GroupThreadID` is lowered as raw integer bits in the
        // internal `vec4<f32>` register model: numeric float conversion would yield float bit
        // patterns and cause out-of-bounds UAV addressing.
        let module = Sm4Module {
            stage: ShaderStage::Compute,
            model: ShaderModel { major: 5, minor: 0 },
            decls: vec![
                Sm4Decl::ThreadGroupSize { x: 2, y: 2, z: 1 },
                Sm4Decl::InputSiv {
                    reg: 0,
                    mask: WriteMask::XYZW,
                    sys_value: D3D_NAME_GROUP_THREAD_ID,
                },
                Sm4Decl::InputSiv {
                    reg: 1,
                    mask: WriteMask::XYZW,
                    sys_value: D3D_NAME_GROUP_ID,
                },
                Sm4Decl::InputSiv {
                    reg: 2,
                    mask: WriteMask::XYZW,
                    sys_value: D3D_NAME_GROUP_INDEX,
                },
                Sm4Decl::UavBuffer {
                    slot: 0,
                    stride: 0,
                    kind: BufferKind::Raw,
                },
            ],
            instructions: vec![
                // r0.x = local_linear = id.x | (id.y << 1)
                Sm4Inst::Bfi {
                    dst: dst(RegFile::Temp, 0, WriteMask::X),
                    width: src_imm_u32(31),
                    offset: src_imm_u32(1),
                    insert: src_reg(RegFile::Input, 0, Swizzle::YYYY),
                    base: src_reg(RegFile::Input, 0, Swizzle::XXXX),
                },
                // r1.x = global = (group_id.x << 2) | group_index
                Sm4Inst::Bfi {
                    dst: dst(RegFile::Temp, 1, WriteMask::X),
                    width: src_imm_u32(30),
                    offset: src_imm_u32(2),
                    insert: src_reg(RegFile::Input, 1, Swizzle::XXXX),
                    base: src_reg(RegFile::Input, 2, Swizzle::XXXX),
                },
                // r2.x = addr = global << 2 (byte address)
                Sm4Inst::Bfi {
                    dst: dst(RegFile::Temp, 2, WriteMask::X),
                    width: src_imm_u32(30),
                    offset: src_imm_u32(2),
                    insert: src_reg(RegFile::Temp, 1, Swizzle::XXXX),
                    base: src_imm_u32(0),
                },
                // store_raw u0.x, r2.x, r0.x
                Sm4Inst::StoreRaw {
                    uav: UavRef { slot: 0 },
                    addr: src_reg(RegFile::Temp, 2, Swizzle::XXXX),
                    value: src_reg(RegFile::Temp, 0, Swizzle::XXXX),
                    mask: WriteMask::X,
                },
                Sm4Inst::Ret,
            ],
        };

        let dxbc_bytes = dummy_dxbc_bytes();
        let dxbc = DxbcFile::parse(&dxbc_bytes).expect("dummy DXBC should parse");
        let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &ShaderSignatures::default())
            .expect("compute translation should succeed");
        assert_wgsl_validates(&translated.wgsl);
        assert!(
            translated.wgsl.contains("@builtin(local_invocation_id)"),
            "expected local_invocation_id builtin in WGSL:\n{}",
            translated.wgsl
        );
        assert!(
            translated.wgsl.contains("@builtin(workgroup_id)"),
            "expected workgroup_id builtin in WGSL:\n{}",
            translated.wgsl
        );
        assert!(
            translated.wgsl.contains("@builtin(local_invocation_index)"),
            "expected local_invocation_index builtin in WGSL:\n{}",
            translated.wgsl
        );
        let binding_u0 = BINDING_BASE_UAV;
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
        w.dispatch(WORKGROUPS_X, 1, 1);
        w.end_compute_pass();
        w.copy_buffer_to_buffer(OUT, 0, READBACK, 0, size_bytes);

        rt.execute(&w.finish()).expect("execute command stream");
        rt.poll_wait();
        let data = rt
            .read_buffer(READBACK, 0, size_bytes)
            .await
            .expect("read buffer");

        let mut got = Vec::<u32>::with_capacity(ELEMENTS as usize);
        for i in 0..ELEMENTS as usize {
            let at = i * 4;
            got.push(u32::from_le_bytes(
                data[at..at + 4].try_into().expect("read 4 bytes"),
            ));
        }
        let mut expected = Vec::<u32>::with_capacity(ELEMENTS as usize);
        for _ in 0..WORKGROUPS_X {
            expected.extend(0..4u32);
        }
        assert_eq!(got, expected);
    });
}

#[test]
fn compute_translate_and_run_compute_builtin_operand_types_write_packed_values() {
    pollster::block_on(async {
        const TEST_NAME: &str = concat!(
            module_path!(),
            "::compute_translate_and_run_compute_builtin_operand_types_write_packed_values"
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
        // This test is sensitive to:
        // - correct mapping of SM5 compute operand types 32..35 to WGSL builtins
        //   (`global_invocation_id`, `workgroup_id`, `local_invocation_id`, `local_invocation_index`)
        // - and correct expansion of those builtins into our untyped `vec4<f32>` register model
        //   as raw integer bits (via `bitcast<f32>`).
        const GROUP_SIZE_X: u32 = 2;
        const GROUP_SIZE_Y: u32 = 2;
        const WORKGROUPS_X: u32 = 2;
        const WORKGROUPS_Y: u32 = 2;
        const GRID_X: u32 = GROUP_SIZE_X * WORKGROUPS_X;
        const GRID_Y: u32 = GROUP_SIZE_Y * WORKGROUPS_Y;
        const INVOCATIONS: u32 = GRID_X * GRID_Y;
        // GRID_X == 4, so `log2(GRID_X) == 2`.
        const LOG2_GRID_X: u32 = 2;
        const INSERT_WIDTH: u32 = 32 - LOG2_GRID_X;

        // Each invocation writes 4 u32 words (16 bytes).
        let size_bytes = (INVOCATIONS as u64) * 16;

        // Build an SM4 IR module that:
        // - computes a unique linear global invocation index from `DispatchThreadId.xy`
        // - uses that index to address a raw UAV buffer
        // - and stores four values sourced from the compute builtin operand types:
        //   [global_linear, group_id.x, group_thread_id.x, group_index].
        let module = Sm4Module {
            stage: ShaderStage::Compute,
            model: ShaderModel { major: 5, minor: 0 },
            decls: vec![
                Sm4Decl::ThreadGroupSize {
                    x: GROUP_SIZE_X,
                    y: GROUP_SIZE_Y,
                    z: 1,
                },
                Sm4Decl::UavBuffer {
                    slot: 0,
                    stride: 0,
                    kind: BufferKind::Raw,
                },
            ],
            instructions: vec![
                // r0.x = global_linear = id.x | (id.y << log2(GRID_X))
                Sm4Inst::Bfi {
                    dst: dst(RegFile::Temp, 0, WriteMask::X),
                    width: src_imm_u32(INSERT_WIDTH),
                    offset: src_imm_u32(LOG2_GRID_X),
                    insert: src_compute_builtin(ComputeBuiltin::DispatchThreadId, Swizzle::YYYY),
                    base: src_compute_builtin(ComputeBuiltin::DispatchThreadId, Swizzle::XXXX),
                },
                // r1.x = addr = global_linear << 4 (byte address for 16-byte records)
                Sm4Inst::Bfi {
                    dst: dst(RegFile::Temp, 1, WriteMask::X),
                    width: src_imm_u32(28),
                    offset: src_imm_u32(4),
                    insert: src_reg(RegFile::Temp, 0, Swizzle::XXXX),
                    base: src_imm_u32(0),
                },
                // r2.x = global_linear
                Sm4Inst::Mov {
                    dst: dst(RegFile::Temp, 2, WriteMask::X),
                    src: src_reg(RegFile::Temp, 0, Swizzle::XXXX),
                },
                // r2.y = group_id.x
                Sm4Inst::Mov {
                    dst: dst(RegFile::Temp, 2, WriteMask::Y),
                    src: src_compute_builtin(ComputeBuiltin::GroupId, Swizzle::XXXX),
                },
                // r2.z = group_thread_id.x
                Sm4Inst::Mov {
                    dst: dst(RegFile::Temp, 2, WriteMask::Z),
                    src: src_compute_builtin(ComputeBuiltin::GroupThreadId, Swizzle::XXXX),
                },
                // r2.w = group_index
                Sm4Inst::Mov {
                    dst: dst(RegFile::Temp, 2, WriteMask::W),
                    src: src_compute_builtin(ComputeBuiltin::GroupIndex, Swizzle::XXXX),
                },
                // store_raw u0.xyzw, r1.x, r2.xyzw
                Sm4Inst::StoreRaw {
                    uav: UavRef { slot: 0 },
                    addr: src_reg(RegFile::Temp, 1, Swizzle::XXXX),
                    value: src_reg(RegFile::Temp, 2, Swizzle::XYZW),
                    mask: WriteMask::XYZW,
                },
                Sm4Inst::Ret,
            ],
        };

        let dxbc_bytes = dummy_dxbc_bytes();
        let dxbc = DxbcFile::parse(&dxbc_bytes).expect("dummy DXBC should parse");
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
