mod common;

use aero_d3d11::binding_model::BINDING_BASE_UAV;
use aero_d3d11::runtime::execute::D3D11Runtime;
use aero_d3d11::{
    translate_sm4_module_to_wgsl, BufferKind, DxbcFile, OperandModifier, ShaderModel, ShaderStage,
    ShaderSignatures, Sm4Decl, Sm4Inst, Sm4Module, SrcKind, SrcOperand, Swizzle, UavRef, WriteMask,
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

#[test]
fn compute_translate_and_run_store_raw_uav_buffer() {
    pollster::block_on(async {
        const TEST_NAME: &str = concat!(
            module_path!(),
            "::compute_translate_and_run_store_raw_uav_buffer"
        );

        let expected: u32 = 0x1234_5678;

        let module = Sm4Module {
            stage: ShaderStage::Compute,
            model: ShaderModel { major: 5, minor: 0 },
            decls: vec![
                Sm4Decl::ThreadGroupSize { x: 1, y: 1, z: 1 },
                Sm4Decl::UavBuffer {
                    slot: 0,
                    stride: 0,
                    kind: BufferKind::Raw,
                },
            ],
            instructions: vec![
                Sm4Inst::StoreRaw {
                    uav: UavRef { slot: 0 },
                    addr: SrcOperand {
                        kind: SrcKind::ImmediateF32([0; 4]),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
                    value: SrcOperand {
                        kind: SrcKind::ImmediateF32([expected, 0, 0, 0]),
                        swizzle: Swizzle::XYZW,
                        modifier: OperandModifier::None,
                    },
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

        let mut rt = match D3D11Runtime::new_for_tests().await {
            Ok(rt) => rt,
            Err(err) => {
                common::skip_or_panic(TEST_NAME, &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };

        const BUF: u32 = 1;
        const SHADER: u32 = 2;
        const PIPELINE: u32 = 3;

        let binding_u0 = BINDING_BASE_UAV + 0;

        let mut w = CmdWriter::new();
        w.create_buffer(
            BUF,
            16,
            BufferUsage::MAP_READ | BufferUsage::STORAGE | BufferUsage::COPY_DST,
        );
        w.update_buffer(BUF, 0, &[0u8; 16]);

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
        w.dispatch(1, 1, 1);
        w.end_compute_pass();

        rt.execute(&w.finish()).expect("execute command stream");
        rt.poll_wait();

        let data = rt.read_buffer(BUF, 0, 4).await.expect("read buffer");
        let got = u32::from_le_bytes(data[..4].try_into().expect("read 4 bytes"));
        assert_eq!(got, expected);
    });
}

