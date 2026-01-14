mod common;

use core::mem::offset_of;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::runtime::bindings::{BoundConstantBuffer, BoundSampler, BoundTexture, ShaderStage};
use aero_dxbc::{test_utils as dxbc_test_utils, FourCC};
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode, AerogpuCmdStreamHeader,
    AerogpuConstantBufferBinding, AerogpuShaderStage, AerogpuShaderStageEx,
    AEROGPU_CMD_STREAM_MAGIC,
};
use aero_protocol::aerogpu::aerogpu_pci::AEROGPU_ABI_VERSION_U32;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

const CMD_STREAM_SIZE_BYTES_OFFSET: usize = offset_of!(AerogpuCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = offset_of!(ProtocolCmdHdr, size_bytes);

fn build_dxbc(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
    dxbc_test_utils::build_container_owned(chunks)
}

fn build_minimal_sm4_program_chunk(program_type: u16) -> Vec<u8> {
    // SM4+ version token layout:
    // - bits 0..=3: minor version
    // - bits 4..=7: major version
    // - bits 16..=31: program type (0=ps, 1=vs, 2=gs, 3=hs, 4=ds, 5=cs)
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

fn push_set_texture(stream: &mut Vec<u8>, stage: u32, slot: u32, texture: u32, stage_ex: u32) {
    let start = begin_cmd(stream, AerogpuCmdOpcode::SetTexture as u32);
    stream.extend_from_slice(&stage.to_le_bytes());
    stream.extend_from_slice(&slot.to_le_bytes());
    stream.extend_from_slice(&texture.to_le_bytes());
    stream.extend_from_slice(&stage_ex.to_le_bytes());
    end_cmd(stream, start);
}

#[test]
fn aerogpu_cmd_stage_ex_bindings_route_to_hs_ds_stage_buckets() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const HS_SHADER: u32 = 1;
        const DS_SHADER: u32 = 2;

        let hs_dxbc = build_dxbc(&[(FourCC(*b"SHEX"), build_minimal_sm4_program_chunk(3))]);
        let ds_dxbc = build_dxbc(&[(FourCC(*b"SHEX"), build_minimal_sm4_program_chunk(4))]);

        let mut w = AerogpuCmdWriter::new();

        w.create_shader_dxbc_ex(HS_SHADER, AerogpuShaderStageEx::Hull, &hs_dxbc);
        w.create_shader_dxbc_ex(DS_SHADER, AerogpuShaderStageEx::Domain, &ds_dxbc);

        // Bind the HS/DS via the extended BIND_SHADERS ABI.
        w.bind_shaders_hs_ds(HS_SHADER, DS_SHADER);

        let cb = |buffer: u32| AerogpuConstantBufferBinding {
            buffer,
            offset_bytes: 0,
            size_bytes: 16,
            reserved0: 0,
        };

        // VS/PS bindings (baseline routing should remain unchanged).
        w.set_constant_buffers(AerogpuShaderStage::Vertex, 1, &[cb(101)]);
        w.set_samplers(AerogpuShaderStage::Vertex, 0, &[201]);
        w.set_texture(AerogpuShaderStage::Vertex, 0, 301);

        w.set_constant_buffers(AerogpuShaderStage::Pixel, 1, &[cb(102)]);
        w.set_samplers(AerogpuShaderStage::Pixel, 0, &[202]);
        w.set_texture(AerogpuShaderStage::Pixel, 0, 302);

        // CS bindings (reserved0==0 is the real compute stage).
        w.set_constant_buffers(AerogpuShaderStage::Compute, 1, &[cb(103)]);
        w.set_samplers(AerogpuShaderStage::Compute, 0, &[203]);
        w.set_texture(AerogpuShaderStage::Compute, 0, 303);

        // HS bindings use shader_stage==COMPUTE + stage_ex reserved0 to select the HS bucket.
        w.set_constant_buffers_ex(AerogpuShaderStageEx::Hull, 1, &[cb(104)]);
        w.set_samplers_ex(AerogpuShaderStageEx::Hull, 0, &[204]);
        w.set_texture_ex(AerogpuShaderStageEx::Hull, 0, 304);

        // DS bindings use shader_stage==COMPUTE + stage_ex reserved0 to select the DS bucket.
        w.set_constant_buffers_ex(AerogpuShaderStageEx::Domain, 1, &[cb(105)]);
        w.set_samplers_ex(AerogpuShaderStageEx::Domain, 0, &[205]);
        w.set_texture_ex(AerogpuShaderStageEx::Domain, 0, 305);

        // Second CS update ensures CS/HS/DS buckets remain distinct in both directions.
        w.set_constant_buffers(AerogpuShaderStage::Compute, 1, &[cb(106)]);
        w.set_samplers(AerogpuShaderStage::Compute, 0, &[206]);
        w.set_texture(AerogpuShaderStage::Compute, 0, 306);

        let stream = w.finish();

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("command stream should execute");

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

        assert_eq!(
            bindings.stage(ShaderStage::Hull).constant_buffer(1),
            expect_cb(104)
        );
        assert_eq!(
            bindings.stage(ShaderStage::Hull).sampler(0),
            Some(BoundSampler { sampler: 204 })
        );
        assert_eq!(
            bindings.stage(ShaderStage::Hull).texture(0),
            Some(BoundTexture { texture: 304 })
        );

        assert_eq!(
            bindings.stage(ShaderStage::Domain).constant_buffer(1),
            expect_cb(105)
        );
        assert_eq!(
            bindings.stage(ShaderStage::Domain).sampler(0),
            Some(BoundSampler { sampler: 205 })
        );
        assert_eq!(
            bindings.stage(ShaderStage::Domain).texture(0),
            Some(BoundTexture { texture: 305 })
        );

        // CS state must remain separate from stage_ex HS/DS updates.
        assert_eq!(
            bindings.stage(ShaderStage::Compute).constant_buffer(1),
            expect_cb(106)
        );
        assert_eq!(
            bindings.stage(ShaderStage::Compute).sampler(0),
            Some(BoundSampler { sampler: 206 })
        );
        assert_eq!(
            bindings.stage(ShaderStage::Compute).texture(0),
            Some(BoundTexture { texture: 306 })
        );
    });
}

#[test]
fn aerogpu_cmd_legacy_hs_ds_stage_ids_are_accepted_for_pre_stage_ex_abi() {
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
        // Patch cmd stream header ABI version from 1.3+ (current) to 1.2 (pre stage_ex).
        stream[4..8].copy_from_slice(&0x0001_0002u32.to_le_bytes());

        // Legacy encoding: some older command streams used shader_stage=4/5 for HS/DS instead of
        // stage_ex (which did not exist yet).
        //
        // Use SET_TEXTURE because it exercises the per-packet stage decoder and updates the
        // binding buckets without requiring actual HS/DS shader execution.
        push_set_texture(&mut stream, 4, 0, 111, 0); // Hull
        push_set_texture(&mut stream, 5, 0, 222, 0); // Domain

        let stream = finish_stream(stream);

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("command stream should execute");

        let bindings = exec.binding_state();
        assert_eq!(
            bindings.stage(ShaderStage::Hull).texture(0),
            Some(BoundTexture { texture: 111 })
        );
        assert_eq!(
            bindings.stage(ShaderStage::Domain).texture(0),
            Some(BoundTexture { texture: 222 })
        );
    });
}
