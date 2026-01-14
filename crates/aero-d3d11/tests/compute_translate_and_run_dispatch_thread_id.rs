mod common;

use aero_d3d11::binding_model::BINDING_BASE_UAV;
use aero_d3d11::runtime::execute::D3D11Runtime;
use aero_d3d11::{
    translate_sm4_module_to_wgsl, BufferKind, DstOperand, DxbcFile, OperandModifier, RegFile,
    RegisterRef, ShaderModel, ShaderSignatures, ShaderStage, Sm4Decl, Sm4Inst, Sm4Module, SrcKind,
    SrcOperand, Swizzle, UavRef, WriteMask,
};
use aero_gpu::protocol_d3d11::{
    BindingDesc, BindingType, BufferUsage, CmdWriter, PipelineKind, ShaderStageFlags,
};

fn dummy_dxbc_bytes() -> Vec<u8> {
    // Minimal DXBC container with no chunks. The signature-driven SM4→WGSL translator only uses the
    // DXBC input for diagnostics, so this is sufficient for compute-stage tests.
    let mut bytes = Vec::with_capacity(32);
    bytes.extend_from_slice(b"DXBC");
    bytes.extend_from_slice(&[0u8; 16]); // checksum (ignored)
    bytes.extend_from_slice(&1u32.to_le_bytes()); // reserved
    bytes.extend_from_slice(&(32u32).to_le_bytes()); // total_size
    bytes.extend_from_slice(&0u32.to_le_bytes()); // chunk_count
    bytes
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

        const ELEMENTS: u32 = 16;
        let size_bytes = (ELEMENTS as u64) * 4;

        // D3D10_SB_NAME_DISPATCH_THREAD_ID.
        const D3D_NAME_DISPATCH_THREAD_ID: u32 = 20;

        // Build an SM4 IR module that writes `SV_DispatchThreadID.x` into a UAV buffer at index
        // `SV_DispatchThreadID.x`.
        //
        // Since `store_raw` takes a byte offset, compute `addr = id.x << 2` using the `bfi`
        // (bitfield insert) instruction:
        //   addr = insertBits(0, id.x, 2, 30)
        //
        // This test is specifically sensitive to whether the builtin is expanded into the untyped
        // `vec4<f32>` register model as *raw integer bits* (via bitcast) rather than float numeric
        // values; numeric conversion would produce float bit patterns and cause out-of-bounds UAV
        // writes.
        let module = Sm4Module {
            stage: ShaderStage::Compute,
            model: ShaderModel { major: 5, minor: 0 },
            decls: vec![
                Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 },
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
                // r0.x = v0.x << 2
                Sm4Inst::Bfi {
                    dst: dst(RegFile::Temp, 0, WriteMask::X),
                    width: src_imm_u32(30),
                    offset: src_imm_u32(2),
                    insert: src_reg(RegFile::Input, 0, Swizzle::XXXX),
                    base: src_imm_u32(0),
                },
                // store_raw u0.x, r0.x, v0.x
                Sm4Inst::StoreRaw {
                    uav: UavRef { slot: 0 },
                    addr: src_reg(RegFile::Temp, 0, Swizzle::XXXX),
                    value: src_reg(RegFile::Input, 0, Swizzle::XXXX),
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

        const BUF: u32 = 1;
        const SHADER: u32 = 2;
        const PIPELINE: u32 = 3;

        let binding_u0 = BINDING_BASE_UAV + 0;

        let zero_init = vec![0u8; size_bytes as usize];
        let mut w = CmdWriter::new();
        w.create_buffer(
            BUF,
            size_bytes,
            BufferUsage::MAP_READ | BufferUsage::STORAGE | BufferUsage::COPY_DST,
        );
        w.update_buffer(BUF, 0, &zero_init);

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
        w.set_bind_buffer(binding_u0, BUF, 0, 0);
        w.begin_compute_pass();
        w.dispatch(ELEMENTS, 1, 1);
        w.end_compute_pass();

        rt.execute(&w.finish()).expect("execute command stream");
        rt.poll_wait();

        let data = rt
            .read_buffer(BUF, 0, size_bytes)
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
