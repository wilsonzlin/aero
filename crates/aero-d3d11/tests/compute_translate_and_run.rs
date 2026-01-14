mod common;

use aero_d3d11::binding_model::{BINDING_BASE_TEXTURE, BINDING_BASE_UAV};
use aero_d3d11::runtime::execute::D3D11Runtime;
use aero_d3d11::sm4::decode_program;
use aero_d3d11::{translate_sm4_module_to_wgsl, DxbcFile, ShaderSignatures, Sm4Program};
use aero_gpu::protocol_d3d11::{
    BindingDesc, BindingType, BufferUsage, CmdWriter, PipelineKind, ShaderStageFlags,
};

// Note: translated compute shaders use `@group(2)` (stage-scoped binding model). The
// `protocol_d3d11` runtime (`D3D11Runtime`) binds compute resources at group 2 so translated WGSL
// can execute through the CmdWriter path.

const CS_STORE_UAV_RAW_DXBC: &[u8] = include_bytes!("fixtures/cs_store_uav_raw.dxbc");
const CS_COPY_RAW_SRV_TO_UAV_DXBC: &[u8] = include_bytes!("fixtures/cs_copy_raw_srv_to_uav.dxbc");
const CS_COPY_STRUCTURED_SRV_TO_UAV_DXBC: &[u8] =
    include_bytes!("fixtures/cs_copy_structured_srv_to_uav.dxbc");

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

        // Use a checked-in DXBC fixture (rather than constructing an Sm4Module manually) so the
        // runtime test covers the full DXBC → decode → WGSL path.
        let dxbc = DxbcFile::parse(CS_STORE_UAV_RAW_DXBC).expect("fixture DXBC should parse");
        let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM5 parse should succeed");
        let module = decode_program(&program).expect("SM5 decode should succeed");

        let translated = translate_sm4_module_to_wgsl(&dxbc, &module, &ShaderSignatures::default())
            .expect("compute translation should succeed");
        assert_wgsl_validates(&translated.wgsl);

        let binding_u0 = BINDING_BASE_UAV + 0;
        assert!(
            translated
                .wgsl
                .contains(&format!("@group(2) @binding({binding_u0})")),
            "expected u0 storage buffer binding to use @group(2); wgsl={}",
            translated.wgsl
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

        const OUT: u32 = 1;
        const READBACK: u32 = 2;
        const SHADER: u32 = 3;
        const PIPELINE: u32 = 4;

        let mut w = CmdWriter::new();
        w.create_buffer(
            OUT,
            16,
            BufferUsage::STORAGE | BufferUsage::COPY_SRC | BufferUsage::COPY_DST,
        );
        w.create_buffer(READBACK, 4, BufferUsage::MAP_READ | BufferUsage::COPY_DST);
        w.update_buffer(OUT, 0, &[0u8; 16]);

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
        w.dispatch(1, 1, 1);
        w.end_compute_pass();

        w.copy_buffer_to_buffer(OUT, 0, READBACK, 0, 4);

        rt.execute(&w.finish()).expect("execute command stream");
        rt.poll_wait();

        let data = rt.read_buffer(READBACK, 0, 4).await.expect("read buffer");
        let got = u32::from_le_bytes(data[..4].try_into().expect("read 4 bytes"));
        assert_eq!(got, expected);
    });
}

#[test]
fn compute_translate_and_run_copy_raw_srv_to_uav() {
    pollster::block_on(async {
        const TEST_NAME: &str = concat!(
            module_path!(),
            "::compute_translate_and_run_copy_raw_srv_to_uav"
        );

        // Include values whose raw bits correspond to non-negative integer floats (e.g. 1.0). The
        // translator must preserve raw bits across `ld_raw`/`store_raw` copies, not reinterpret
        // them as numeric integers.
        let src_words: [u32; 4] = [
            0x3f80_0000, // 1.0f32
            0x4000_0000, // 2.0f32
            0x4040_0000, // 3.0f32
            0x0000_0001, // u32=1 (tiny subnormal if interpreted as f32)
        ];

        // Use a checked-in DXBC fixture to exercise the full DXBC → decode → WGSL path.
        let dxbc = DxbcFile::parse(CS_COPY_RAW_SRV_TO_UAV_DXBC).expect("fixture DXBC should parse");
        let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM5 parse should succeed");
        let module = decode_program(&program).expect("SM5 decode should succeed");

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
        if !rt.supports_compute() {
            common::skip_or_panic(TEST_NAME, "compute unsupported");
            return;
        }

        let binding_t0 = BINDING_BASE_TEXTURE + 0;
        let binding_u0 = BINDING_BASE_UAV + 0;
        assert!(
            translated.wgsl.contains("@group(2)"),
            "translated compute WGSL must use @group(2):\n{}",
            translated.wgsl
        );

        const SRV: u32 = 1;
        const UAV: u32 = 2;
        const READBACK: u32 = 3;
        const SHADER: u32 = 4;
        const PIPELINE: u32 = 5;

        let mut w = CmdWriter::new();
        w.create_buffer(SRV, 16, BufferUsage::STORAGE | BufferUsage::COPY_DST);
        w.create_buffer(
            UAV,
            16,
            BufferUsage::STORAGE | BufferUsage::COPY_SRC | BufferUsage::COPY_DST,
        );
        w.create_buffer(READBACK, 16, BufferUsage::MAP_READ | BufferUsage::COPY_DST);
        w.update_buffer(SRV, 0, bytemuck::cast_slice(&src_words));
        w.update_buffer(UAV, 0, &[0u8; 16]);

        w.create_shader_module_wgsl(SHADER, &translated.wgsl);
        w.create_compute_pipeline(
            PIPELINE,
            SHADER,
            &[
                BindingDesc {
                    binding: binding_t0,
                    ty: BindingType::StorageBufferReadOnly,
                    visibility: ShaderStageFlags::COMPUTE,
                    storage_texture_format: None,
                },
                BindingDesc {
                    binding: binding_u0,
                    ty: BindingType::StorageBufferReadWrite,
                    visibility: ShaderStageFlags::COMPUTE,
                    storage_texture_format: None,
                },
            ],
        );

        w.set_pipeline(PipelineKind::Compute, PIPELINE);
        w.set_bind_buffer(binding_t0, SRV, 0, 0);
        w.set_bind_buffer(binding_u0, UAV, 0, 0);

        w.begin_compute_pass();
        w.dispatch(1, 1, 1);
        w.end_compute_pass();
        w.copy_buffer_to_buffer(UAV, 0, READBACK, 0, 16);

        rt.execute(&w.finish()).expect("execute command stream");
        rt.poll_wait();
        let got = rt.read_buffer(READBACK, 0, 16).await.expect("read buffer");
        assert_eq!(got.as_slice(), bytemuck::cast_slice::<u32, u8>(&src_words));
    });
}

#[test]
fn compute_translate_and_run_copy_structured_srv_to_uav() {
    pollster::block_on(async {
        const TEST_NAME: &str = concat!(
            module_path!(),
            "::compute_translate_and_run_copy_structured_srv_to_uav"
        );

        // Two 16-byte elements (8 u32s). We'll read element 1 and write it into element 0.
        //
        // Use float bit patterns for element 1 so the test covers the "preserve raw bits" behavior
        // when values look like non-negative integer floats (e.g. 1.0).
        let src_words: [u32; 8] = [
            0,
            1,
            2,
            3,           // element 0
            0x3f80_0000, // 1.0f32
            0x4000_0000, // 2.0f32
            0x4040_0000, // 3.0f32
            0x4080_0000, // 4.0f32
        ];
        let expected: [u32; 4] = [0x3f80_0000, 0x4000_0000, 0x4040_0000, 0x4080_0000];

        // Use a checked-in DXBC fixture to exercise the full DXBC → decode → WGSL path.
        let dxbc = DxbcFile::parse(CS_COPY_STRUCTURED_SRV_TO_UAV_DXBC)
            .expect("fixture DXBC should parse");
        let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM5 parse should succeed");
        let module = decode_program(&program).expect("SM5 decode should succeed");

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
        if !rt.supports_compute() {
            common::skip_or_panic(TEST_NAME, "compute unsupported");
            return;
        }

        let binding_t0 = BINDING_BASE_TEXTURE + 0;
        let binding_u0 = BINDING_BASE_UAV + 0;
        assert!(
            translated.wgsl.contains("@group(2)"),
            "translated compute WGSL must use @group(2):\n{}",
            translated.wgsl
        );

        const SRV: u32 = 1;
        const UAV: u32 = 2;
        const READBACK: u32 = 3;
        const SHADER: u32 = 4;
        const PIPELINE: u32 = 5;

        let mut w = CmdWriter::new();
        w.create_buffer(SRV, 32, BufferUsage::STORAGE | BufferUsage::COPY_DST);
        w.create_buffer(
            UAV,
            32,
            BufferUsage::STORAGE | BufferUsage::COPY_SRC | BufferUsage::COPY_DST,
        );
        w.create_buffer(READBACK, 16, BufferUsage::MAP_READ | BufferUsage::COPY_DST);
        w.update_buffer(SRV, 0, bytemuck::cast_slice(&src_words));
        w.update_buffer(UAV, 0, &[0u8; 32]);

        w.create_shader_module_wgsl(SHADER, &translated.wgsl);
        w.create_compute_pipeline(
            PIPELINE,
            SHADER,
            &[
                BindingDesc {
                    binding: binding_t0,
                    ty: BindingType::StorageBufferReadOnly,
                    visibility: ShaderStageFlags::COMPUTE,
                    storage_texture_format: None,
                },
                BindingDesc {
                    binding: binding_u0,
                    ty: BindingType::StorageBufferReadWrite,
                    visibility: ShaderStageFlags::COMPUTE,
                    storage_texture_format: None,
                },
            ],
        );

        w.set_pipeline(PipelineKind::Compute, PIPELINE);
        w.set_bind_buffer(binding_t0, SRV, 0, 0);
        w.set_bind_buffer(binding_u0, UAV, 0, 0);
        w.begin_compute_pass();
        w.dispatch(1, 1, 1);
        w.end_compute_pass();

        w.copy_buffer_to_buffer(UAV, 0, READBACK, 0, 16);

        rt.execute(&w.finish()).expect("execute command stream");
        rt.poll_wait();
        let got = rt.read_buffer(READBACK, 0, 16).await.expect("read buffer");
        assert_eq!(got.as_slice(), bytemuck::cast_slice::<u32, u8>(&expected));
    });
}
