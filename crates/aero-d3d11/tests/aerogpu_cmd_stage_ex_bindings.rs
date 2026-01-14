mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::runtime::bindings::{
    BoundBuffer, BoundConstantBuffer, BoundSampler, BoundTexture, ShaderStage,
};
use aero_dxbc::{test_utils as dxbc_test_utils, FourCC};
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode, AerogpuCmdStreamHeader,
    AerogpuSamplerAddressMode, AerogpuSamplerFilter, AerogpuShaderResourceBufferBinding,
    AerogpuShaderStage, AerogpuShaderStageEx, AerogpuUnorderedAccessBufferBinding,
    AEROGPU_CMD_STREAM_MAGIC, AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER, AEROGPU_RESOURCE_USAGE_TEXTURE,
};
use aero_protocol::aerogpu::aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32};
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(AerogpuCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(ProtocolCmdHdr, size_bytes);

// `stage_ex` values use DXBC program-type numbering (SM4/SM5 version token).
//
// Note: DXBC program types 0/1 (Pixel/Vertex) are intentionally invalid in AeroGPU's stage_ex
// encoding; those stages must be represented via the legacy `shader_stage` field.
const STAGE_EX_INVALID_VERTEX: u32 = 1;
const STAGE_EX_GEOMETRY: u32 = 2;
const STAGE_EX_HULL: u32 = 3;
const STAGE_EX_DOMAIN: u32 = 4;

fn align4(len: usize) -> usize {
    (len + 3) & !3
}

fn begin_cmd(stream: &mut Vec<u8>, opcode: u32) -> usize {
    let start = stream.len();
    stream.extend_from_slice(&opcode.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // size placeholder
    start
}

fn end_cmd(stream: &mut [u8], start: usize) {
    let size = (stream.len() - start) as u32;
    stream[start + CMD_HDR_SIZE_BYTES_OFFSET..start + CMD_HDR_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size.to_le_bytes());
    assert_eq!(size % 4, 0, "command not 4-byte aligned");
}

fn finish_stream(mut stream: Vec<u8>) -> Vec<u8> {
    let total_size = stream.len() as u32;
    stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&total_size.to_le_bytes());
    stream
}

fn build_dxbc(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
    dxbc_test_utils::build_container_owned(chunks)
}

fn build_minimal_sm4_program_chunk(program_type: u16) -> Vec<u8> {
    // SM4+ version token layout:
    // - bits 0..=3: minor version
    // - bits 4..=7: major version
    // - bits 16..=31: program type (0=ps, 1=vs, 2=gs, ...)
    let major = 4u32;
    let minor = 0u32;
    let version = (program_type as u32) << 16 | (major << 4) | minor;

    // Declared length in DWORDs includes the version + length tokens.
    let declared_len = 2u32;

    let mut bytes = Vec::with_capacity(8);
    bytes.extend_from_slice(&version.to_le_bytes());
    bytes.extend_from_slice(&declared_len.to_le_bytes());
    bytes
}

fn push_bind_shaders(stream: &mut Vec<u8>, vs: u32, ps: u32, cs: u32, gs: u32, hs: u32, ds: u32) {
    // Encode the append-only `BIND_SHADERS` extension so the executor sees `{gs,hs,ds}` via the
    // canonical decoder (see `drivers/aerogpu/protocol/aerogpu_cmd.h`).
    //
    // Use `AerogpuCmdWriter` here so packet sizing/padding stays correct and consistent across
    // tests/fixtures.
    let mut w = AerogpuCmdWriter::new();
    w.bind_shaders_ex(vs, ps, cs, gs, hs, ds);
    let packet_stream = w.finish();
    stream.extend_from_slice(&packet_stream[AerogpuCmdStreamHeader::SIZE_BYTES..]);
}

fn push_create_texture2d(stream: &mut Vec<u8>, texture: u32) {
    let start = begin_cmd(stream, AerogpuCmdOpcode::CreateTexture2d as u32);
    stream.extend_from_slice(&texture.to_le_bytes());
    stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
    stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
    stream.extend_from_slice(&1u32.to_le_bytes()); // width
    stream.extend_from_slice(&1u32.to_le_bytes()); // height
    stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
    stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
    stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
    stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
    stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
    stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
    end_cmd(stream, start);
}

fn push_create_sampler(stream: &mut Vec<u8>, sampler: u32) {
    let start = begin_cmd(stream, AerogpuCmdOpcode::CreateSampler as u32);
    stream.extend_from_slice(&sampler.to_le_bytes());
    stream.extend_from_slice(&(AerogpuSamplerFilter::Nearest as u32).to_le_bytes());
    stream.extend_from_slice(&(AerogpuSamplerAddressMode::ClampToEdge as u32).to_le_bytes());
    stream.extend_from_slice(&(AerogpuSamplerAddressMode::ClampToEdge as u32).to_le_bytes());
    stream.extend_from_slice(&(AerogpuSamplerAddressMode::ClampToEdge as u32).to_le_bytes());
    end_cmd(stream, start);
}

fn push_create_buffer(stream: &mut Vec<u8>, buffer: u32) {
    let start = begin_cmd(stream, AerogpuCmdOpcode::CreateBuffer as u32);
    stream.extend_from_slice(&buffer.to_le_bytes());
    stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER.to_le_bytes());
    stream.extend_from_slice(&64u64.to_le_bytes()); // size_bytes
    stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
    stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
    stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
    end_cmd(stream, start);
}

fn push_set_texture(stream: &mut Vec<u8>, stage: u32, slot: u32, texture: u32, stage_ex: u32) {
    let start = begin_cmd(stream, AerogpuCmdOpcode::SetTexture as u32);
    stream.extend_from_slice(&stage.to_le_bytes());
    stream.extend_from_slice(&slot.to_le_bytes());
    stream.extend_from_slice(&texture.to_le_bytes());
    stream.extend_from_slice(&stage_ex.to_le_bytes());
    end_cmd(stream, start);
}

fn push_set_samplers(stream: &mut Vec<u8>, stage: u32, slot: u32, sampler: u32, stage_ex: u32) {
    let start = begin_cmd(stream, AerogpuCmdOpcode::SetSamplers as u32);
    stream.extend_from_slice(&stage.to_le_bytes());
    stream.extend_from_slice(&slot.to_le_bytes()); // start_slot
    stream.extend_from_slice(&1u32.to_le_bytes()); // sampler_count
    stream.extend_from_slice(&stage_ex.to_le_bytes());
    stream.extend_from_slice(&sampler.to_le_bytes());
    end_cmd(stream, start);
}

fn push_set_constant_buffer(
    stream: &mut Vec<u8>,
    stage: u32,
    slot: u32,
    buffer: u32,
    stage_ex: u32,
) {
    let start = begin_cmd(stream, AerogpuCmdOpcode::SetConstantBuffers as u32);
    stream.extend_from_slice(&stage.to_le_bytes());
    stream.extend_from_slice(&slot.to_le_bytes()); // start_slot
    stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
    stream.extend_from_slice(&stage_ex.to_le_bytes());

    // struct aerogpu_constant_buffer_binding
    stream.extend_from_slice(&buffer.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
    stream.extend_from_slice(&16u32.to_le_bytes()); // size_bytes
    stream.extend_from_slice(&0u32.to_le_bytes()); // binding reserved0
    end_cmd(stream, start);
}

#[test]
fn aerogpu_cmd_stage_ex_bindings_route_to_correct_stage_bucket() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const GS_SHADER: u32 = 1;
        const HS_SHADER: u32 = 2;
        const DS_SHADER: u32 = 3;

        let gs_dxbc = build_dxbc(&[(FourCC(*b"SHEX"), build_minimal_sm4_program_chunk(2))]);
        let hs_dxbc = build_dxbc(&[(FourCC(*b"SHEX"), build_minimal_sm4_program_chunk(3))]);
        let ds_dxbc = build_dxbc(&[(FourCC(*b"SHEX"), build_minimal_sm4_program_chunk(4))]);

        let mut stream = vec![0u8; AerogpuCmdStreamHeader::SIZE_BYTES];
        stream[0..4].copy_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream[4..8].copy_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());

        // CREATE_SHADER_DXBC (geometry shader payload, SM4 program type 2).
        // Use the `stage_ex` reserved field so the executor sees a GS handle flowing through the
        // stage_ex ABI, but the test does not require actual GS execution.
        {
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
            stream.extend_from_slice(&GS_SHADER.to_le_bytes());
            stream.extend_from_slice(&(AerogpuShaderStage::Compute as u32).to_le_bytes()); // stage
            stream.extend_from_slice(&(gs_dxbc.len() as u32).to_le_bytes());
            stream.extend_from_slice(&STAGE_EX_GEOMETRY.to_le_bytes()); // reserved0 = stage_ex
            stream.extend_from_slice(&gs_dxbc);
            stream.resize(align4(stream.len()), 0);
            end_cmd(&mut stream, start);
        }

        // CREATE_SHADER_DXBC (hull shader payload, SM4 program type 3).
        {
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
            stream.extend_from_slice(&HS_SHADER.to_le_bytes());
            stream.extend_from_slice(&(AerogpuShaderStage::Compute as u32).to_le_bytes()); // stage
            stream.extend_from_slice(&(hs_dxbc.len() as u32).to_le_bytes());
            stream.extend_from_slice(&STAGE_EX_HULL.to_le_bytes()); // reserved0 = stage_ex
            stream.extend_from_slice(&hs_dxbc);
            stream.resize(align4(stream.len()), 0);
            end_cmd(&mut stream, start);
        }

        // CREATE_SHADER_DXBC (domain shader payload, SM4 program type 4).
        {
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
            stream.extend_from_slice(&DS_SHADER.to_le_bytes());
            stream.extend_from_slice(&(AerogpuShaderStage::Compute as u32).to_le_bytes()); // stage
            stream.extend_from_slice(&(ds_dxbc.len() as u32).to_le_bytes());
            stream.extend_from_slice(&STAGE_EX_DOMAIN.to_le_bytes()); // reserved0 = stage_ex
            stream.extend_from_slice(&ds_dxbc);
            stream.resize(align4(stream.len()), 0);
            end_cmd(&mut stream, start);
        }

        // Bind the GS/HS/DS via the append-only `BIND_SHADERS` extension (`{gs,hs,ds}` payload).
        push_bind_shaders(&mut stream, 0, 0, 0, GS_SHADER, HS_SHADER, DS_SHADER);

        // Create dummy resources for each binding table.
        for texture in [301u32, 302, 303, 304, 306, 308] {
            push_create_texture2d(&mut stream, texture);
        }
        for sampler in [201u32, 202, 203, 204, 206, 208] {
            push_create_sampler(&mut stream, sampler);
        }
        for buffer in [101u32, 102, 103, 104, 106, 108] {
            push_create_buffer(&mut stream, buffer);
        }

        // VS/PS bindings (baseline routing should remain unchanged).
        push_set_constant_buffer(&mut stream, AerogpuShaderStage::Vertex as u32, 1, 101, 0);
        push_set_samplers(&mut stream, AerogpuShaderStage::Vertex as u32, 0, 201, 0);
        push_set_texture(&mut stream, AerogpuShaderStage::Vertex as u32, 0, 301, 0);

        push_set_constant_buffer(&mut stream, AerogpuShaderStage::Pixel as u32, 1, 102, 0);
        push_set_samplers(&mut stream, AerogpuShaderStage::Pixel as u32, 0, 202, 0);
        push_set_texture(&mut stream, AerogpuShaderStage::Pixel as u32, 0, 302, 0);

        // Vertex stage bindings are encoded directly via `shader_stage=VERTEX` (not stage_ex).
        push_set_texture(&mut stream, AerogpuShaderStage::Vertex as u32, 2, 304, 0);

        // CS bindings (reserved0==0 is the real compute stage). Write slot 0 first; stage_ex writes
        // must not clobber it.
        push_set_constant_buffer(&mut stream, AerogpuShaderStage::Compute as u32, 0, 103, 0);
        push_set_samplers(&mut stream, AerogpuShaderStage::Compute as u32, 0, 203, 0);
        push_set_texture(&mut stream, AerogpuShaderStage::Compute as u32, 0, 303, 0);

        // GS bindings use shader_stage==COMPUTE + stage_ex reserved0 to select the GS bucket.
        push_set_constant_buffer(
            &mut stream,
            AerogpuShaderStage::Compute as u32,
            0,
            104,
            STAGE_EX_GEOMETRY,
        );
        push_set_samplers(
            &mut stream,
            AerogpuShaderStage::Compute as u32,
            0,
            204,
            STAGE_EX_GEOMETRY,
        );
        push_set_texture(
            &mut stream,
            AerogpuShaderStage::Compute as u32,
            0,
            304,
            STAGE_EX_GEOMETRY,
        );

        // HS/DS bindings also use shader_stage==COMPUTE + stage_ex.
        push_set_constant_buffer(
            &mut stream,
            AerogpuShaderStage::Compute as u32,
            0,
            106,
            STAGE_EX_HULL,
        );
        push_set_samplers(
            &mut stream,
            AerogpuShaderStage::Compute as u32,
            0,
            206,
            STAGE_EX_HULL,
        );
        push_set_texture(
            &mut stream,
            AerogpuShaderStage::Compute as u32,
            0,
            306,
            STAGE_EX_HULL,
        );

        push_set_constant_buffer(
            &mut stream,
            AerogpuShaderStage::Compute as u32,
            0,
            108,
            STAGE_EX_DOMAIN,
        );
        push_set_samplers(
            &mut stream,
            AerogpuShaderStage::Compute as u32,
            0,
            208,
            STAGE_EX_DOMAIN,
        );
        push_set_texture(
            &mut stream,
            AerogpuShaderStage::Compute as u32,
            0,
            308,
            STAGE_EX_DOMAIN,
        );

        // Second CS update writes slot 1 and must not clobber any stage_ex bindings written to
        // slot 1.
        for stage_ex in [STAGE_EX_GEOMETRY, STAGE_EX_HULL, STAGE_EX_DOMAIN] {
            let (cb, samp, tex) = match stage_ex {
                STAGE_EX_GEOMETRY => (104u32, 204u32, 304u32),
                STAGE_EX_HULL => (106u32, 206u32, 306u32),
                STAGE_EX_DOMAIN => (108u32, 208u32, 308u32),
                _ => unreachable!(),
            };

            push_set_constant_buffer(
                &mut stream,
                AerogpuShaderStage::Compute as u32,
                1,
                cb,
                stage_ex,
            );
            push_set_samplers(
                &mut stream,
                AerogpuShaderStage::Compute as u32,
                1,
                samp,
                stage_ex,
            );
            push_set_texture(
                &mut stream,
                AerogpuShaderStage::Compute as u32,
                1,
                tex,
                stage_ex,
            );
        }

        push_set_constant_buffer(&mut stream, AerogpuShaderStage::Compute as u32, 1, 103, 0);
        push_set_samplers(&mut stream, AerogpuShaderStage::Compute as u32, 1, 203, 0);
        push_set_texture(&mut stream, AerogpuShaderStage::Compute as u32, 1, 303, 0);

        let stream = finish_stream(stream);

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("command stream should execute");

        assert_eq!(
            exec.shader_entry_point(GS_SHADER).unwrap(),
            "gs_main",
            "geometry shader should be stored in the shader table"
        );
        assert_eq!(exec.shader_entry_point(HS_SHADER).unwrap(), "hs_main");
        assert_eq!(exec.shader_entry_point(DS_SHADER).unwrap(), "ds_main");

        let bindings = exec.binding_state();

        let expect_cb = |buffer: u32| {
            Some(BoundConstantBuffer {
                buffer,
                offset: 0,
                size: Some(16),
            })
        };

        assert_eq!(
            bindings.stage(ShaderStage::Vertex).constant_buffer(1),
            expect_cb(101)
        );
        assert_eq!(
            bindings.stage(ShaderStage::Vertex).sampler(0),
            Some(BoundSampler { sampler: 201 })
        );
        assert_eq!(
            bindings.stage(ShaderStage::Vertex).texture(0),
            Some(BoundTexture { texture: 301 })
        );
        assert_eq!(
            bindings.stage(ShaderStage::Vertex).texture(2),
            Some(BoundTexture { texture: 304 })
        );
        assert_eq!(bindings.stage(ShaderStage::Compute).texture(2), None);

        assert_eq!(
            bindings.stage(ShaderStage::Pixel).constant_buffer(1),
            expect_cb(102)
        );
        assert_eq!(
            bindings.stage(ShaderStage::Pixel).sampler(0),
            Some(BoundSampler { sampler: 202 })
        );
        assert_eq!(
            bindings.stage(ShaderStage::Pixel).texture(0),
            Some(BoundTexture { texture: 302 })
        );

        // GS updates must not clobber VS/PS.
        assert_eq!(
            bindings.stage(ShaderStage::Geometry).constant_buffer(1),
            expect_cb(104)
        );
        assert_eq!(
            bindings.stage(ShaderStage::Geometry).sampler(0),
            Some(BoundSampler { sampler: 204 })
        );
        assert_eq!(
            bindings.stage(ShaderStage::Geometry).texture(0),
            Some(BoundTexture { texture: 304 })
        );

        assert_eq!(
            bindings.stage(ShaderStage::Hull).constant_buffer(0),
            expect_cb(106)
        );
        assert_eq!(
            bindings.stage(ShaderStage::Hull).sampler(0),
            Some(BoundSampler { sampler: 206 })
        );
        assert_eq!(
            bindings.stage(ShaderStage::Hull).texture(0),
            Some(BoundTexture { texture: 306 })
        );

        assert_eq!(
            bindings.stage(ShaderStage::Domain).constant_buffer(0),
            expect_cb(108)
        );
        assert_eq!(
            bindings.stage(ShaderStage::Domain).sampler(0),
            Some(BoundSampler { sampler: 208 })
        );
        assert_eq!(
            bindings.stage(ShaderStage::Domain).texture(0),
            Some(BoundTexture { texture: 308 })
        );

        // CS state must remain separate from stage_ex GS/HS/DS updates.
        assert_eq!(
            bindings.stage(ShaderStage::Compute).constant_buffer(0),
            expect_cb(103)
        );
        assert_eq!(
            bindings.stage(ShaderStage::Compute).sampler(0),
            Some(BoundSampler { sampler: 203 })
        );
        assert_eq!(
            bindings.stage(ShaderStage::Compute).texture(0),
            Some(BoundTexture { texture: 303 })
        );

        // And CS writes must not clobber stage_ex bindings in the same slot number.
        assert_eq!(
            bindings.stage(ShaderStage::Geometry).constant_buffer(1),
            expect_cb(104)
        );
        assert_eq!(
            bindings.stage(ShaderStage::Geometry).sampler(1),
            Some(BoundSampler { sampler: 204 })
        );
        assert_eq!(
            bindings.stage(ShaderStage::Geometry).texture(1),
            Some(BoundTexture { texture: 304 })
        );

        assert_eq!(
            bindings.stage(ShaderStage::Hull).constant_buffer(1),
            expect_cb(106)
        );
        assert_eq!(
            bindings.stage(ShaderStage::Hull).sampler(1),
            Some(BoundSampler { sampler: 206 })
        );
        assert_eq!(
            bindings.stage(ShaderStage::Hull).texture(1),
            Some(BoundTexture { texture: 306 })
        );

        assert_eq!(
            bindings.stage(ShaderStage::Domain).constant_buffer(1),
            expect_cb(108)
        );
        assert_eq!(
            bindings.stage(ShaderStage::Domain).sampler(1),
            Some(BoundSampler { sampler: 208 })
        );
        assert_eq!(
            bindings.stage(ShaderStage::Domain).texture(1),
            Some(BoundTexture { texture: 308 })
        );

        assert_eq!(
            bindings.stage(ShaderStage::Compute).constant_buffer(1),
            expect_cb(103)
        );
        assert_eq!(
            bindings.stage(ShaderStage::Compute).sampler(1),
            Some(BoundSampler { sampler: 203 })
        );
        assert_eq!(
            bindings.stage(ShaderStage::Compute).texture(1),
            Some(BoundTexture { texture: 303 })
        );
    });
}

#[test]
fn aerogpu_cmd_stage_ex_vertex_value_is_rejected() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let mut stream = vec![0u8; AerogpuCmdStreamHeader::SIZE_BYTES];
        stream[0..4].copy_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream[4..8].copy_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());

        push_create_texture2d(&mut stream, 1);
        // stage_ex=1 is the DXBC program type for Vertex, but it is intentionally invalid in the
        // AeroGPU stage_ex encoding (0 is reserved for legacy/default compute; Vertex must use the
        // legacy stage field).
        push_set_texture(
            &mut stream,
            AerogpuShaderStage::Compute as u32,
            0,
            1,
            STAGE_EX_INVALID_VERTEX,
        );
        let stream = finish_stream(stream);

        let mut guest_mem = VecGuestMemory::new(0);
        let err = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect_err("stage_ex=1 must be rejected");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("stage_ex=1"),
            "error should mention invalid stage_ex=1, got: {msg}"
        );
    });
}

#[test]
fn aerogpu_cmd_legacy_geometry_stage_bindings_update_geometry_bucket() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let mut w = AerogpuCmdWriter::new();

        // Baseline: bind CS so we can assert that GS bindings don't clobber CS state.
        w.set_texture(AerogpuShaderStage::Compute, 0, 111);

        // Legacy GS encoding uses `shader_stage=GEOMETRY` directly (not the stage_ex encoding).
        w.set_texture(AerogpuShaderStage::Geometry, 0, 222);
        w.set_samplers(AerogpuShaderStage::Geometry, 0, &[333]);
        w.set_constant_buffers(
            AerogpuShaderStage::Geometry,
            0,
            &[aero_protocol::aerogpu::aerogpu_cmd::AerogpuConstantBufferBinding {
                buffer: 444,
                offset_bytes: 0,
                size_bytes: 16,
                reserved0: 0,
            }],
        );
        w.set_shader_resource_buffers(
            AerogpuShaderStage::Geometry,
            1,
            &[AerogpuShaderResourceBufferBinding {
                buffer: 555,
                offset_bytes: 0,
                size_bytes: 0,
                reserved0: 0,
            }],
        );
        w.set_unordered_access_buffers(
            AerogpuShaderStage::Geometry,
            0,
            &[AerogpuUnorderedAccessBufferBinding {
                buffer: 666,
                offset_bytes: 0,
                size_bytes: 0,
                initial_count: 0,
            }],
        );

        let stream = w.finish();

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("command stream should execute");

        let bindings = exec.binding_state();
        assert_eq!(
            bindings.stage(ShaderStage::Compute).texture(0),
            Some(BoundTexture { texture: 111 })
        );

        assert_eq!(
            bindings.stage(ShaderStage::Geometry).texture(0),
            Some(BoundTexture { texture: 222 })
        );
        assert_eq!(
            bindings.stage(ShaderStage::Geometry).sampler(0),
            Some(BoundSampler { sampler: 333 })
        );
        assert_eq!(
            bindings.stage(ShaderStage::Geometry).constant_buffer(0),
            Some(BoundConstantBuffer {
                buffer: 444,
                offset: 0,
                size: Some(16),
            })
        );
        assert_eq!(
            bindings.stage(ShaderStage::Geometry).srv_buffer(1),
            Some(BoundBuffer {
                buffer: 555,
                offset: 0,
                size: None,
            })
        );
        assert_eq!(
            bindings.stage(ShaderStage::Geometry).uav_buffer(0),
            Some(BoundBuffer {
                buffer: 666,
                offset: 0,
                size: None,
            })
        );
    });
}

#[test]
fn aerogpu_cmd_buffer_bindings_update_stage_state_and_unbind_t_slot_conflicts() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let mut w = AerogpuCmdWriter::new();

        // `t0` can be bound as either a texture or an SRV buffer; binding one kind must unbind the
        // other.
        w.set_texture(AerogpuShaderStage::Vertex, 0, 111);
        w.set_shader_resource_buffers(
            AerogpuShaderStage::Vertex,
            0,
            &[AerogpuShaderResourceBufferBinding {
                buffer: 222,
                offset_bytes: 0,
                size_bytes: 0,
                reserved0: 0,
            }],
        );

        w.set_shader_resource_buffers(
            AerogpuShaderStage::Pixel,
            1,
            &[AerogpuShaderResourceBufferBinding {
                buffer: 333,
                offset_bytes: 0,
                size_bytes: 0,
                reserved0: 0,
            }],
        );
        w.set_texture(AerogpuShaderStage::Pixel, 1, 444);

        // Compute-stage buffer bindings.
        w.set_shader_resource_buffers(
            AerogpuShaderStage::Compute,
            2,
            &[AerogpuShaderResourceBufferBinding {
                buffer: 555,
                offset_bytes: 0,
                size_bytes: 0,
                reserved0: 0,
            }],
        );
        w.set_unordered_access_buffers(
            AerogpuShaderStage::Compute,
            0,
            &[AerogpuUnorderedAccessBufferBinding {
                buffer: 666,
                offset_bytes: 0,
                size_bytes: 0,
                initial_count: 0,
            }],
        );

        // GS bindings use shader_stage==COMPUTE + stage_ex reserved0 to select the GS bucket.
        w.set_shader_resource_buffers_ex(
            AerogpuShaderStageEx::Geometry,
            2,
            &[AerogpuShaderResourceBufferBinding {
                buffer: 777,
                offset_bytes: 0,
                size_bytes: 0,
                reserved0: 0,
            }],
        );
        w.set_unordered_access_buffers_ex(
            AerogpuShaderStageEx::Geometry,
            0,
            &[AerogpuUnorderedAccessBufferBinding {
                buffer: 888,
                offset_bytes: 0,
                size_bytes: 0,
                initial_count: 0,
            }],
        );

        let stream = w.finish();

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("command stream should execute");

        let bindings = exec.binding_state();

        assert_eq!(
            bindings.stage(ShaderStage::Vertex).srv_buffer(0),
            Some(BoundBuffer {
                buffer: 222,
                offset: 0,
                size: None,
            })
        );
        assert_eq!(bindings.stage(ShaderStage::Vertex).texture(0), None);

        assert_eq!(
            bindings.stage(ShaderStage::Pixel).texture(1),
            Some(BoundTexture { texture: 444 })
        );
        assert_eq!(bindings.stage(ShaderStage::Pixel).srv_buffer(1), None);

        assert_eq!(
            bindings.stage(ShaderStage::Compute).srv_buffer(2),
            Some(BoundBuffer {
                buffer: 555,
                offset: 0,
                size: None,
            })
        );
        assert_eq!(
            bindings.stage(ShaderStage::Compute).uav_buffer(0),
            Some(BoundBuffer {
                buffer: 666,
                offset: 0,
                size: None,
            })
        );

        assert_eq!(
            bindings.stage(ShaderStage::Geometry).srv_buffer(2),
            Some(BoundBuffer {
                buffer: 777,
                offset: 0,
                size: None,
            })
        );
        assert_eq!(
            bindings.stage(ShaderStage::Geometry).uav_buffer(0),
            Some(BoundBuffer {
                buffer: 888,
                offset: 0,
                size: None,
            })
        );
    });
}
