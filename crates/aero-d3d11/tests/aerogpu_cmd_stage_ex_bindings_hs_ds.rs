mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::runtime::bindings::{BoundConstantBuffer, BoundSampler, BoundTexture, ShaderStage};
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode, AerogpuCmdStreamHeader,
    AerogpuShaderStage, AEROGPU_CMD_STREAM_MAGIC,
};
use aero_protocol::aerogpu::aerogpu_pci::AEROGPU_ABI_VERSION_U32;

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(AerogpuCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(ProtocolCmdHdr, size_bytes);

// `stage_ex` values use DXBC program-type numbering (SM4/SM5 version token).
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

fn build_dxbc(chunks: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let chunk_count = u32::try_from(chunks.len()).expect("too many chunks for test DXBC");
    let header_len = 4 + 16 + 4 + 4 + 4 + (chunks.len() * 4);

    let mut offsets = Vec::with_capacity(chunks.len());
    let mut cursor = header_len;
    for (_fourcc, data) in chunks {
        offsets.push(cursor as u32);
        cursor += 8 + data.len();
    }
    let total_size = cursor as u32;

    let mut bytes = Vec::with_capacity(cursor);
    bytes.extend_from_slice(b"DXBC");
    bytes.extend_from_slice(&[0u8; 16]); // checksum (ignored)
    bytes.extend_from_slice(&1u32.to_le_bytes()); // reserved/unknown
    bytes.extend_from_slice(&total_size.to_le_bytes());
    bytes.extend_from_slice(&chunk_count.to_le_bytes());
    for off in offsets {
        bytes.extend_from_slice(&off.to_le_bytes());
    }
    for (fourcc, data) in chunks {
        bytes.extend_from_slice(fourcc);
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(data);
    }
    assert_eq!(bytes.len(), total_size as usize);
    bytes
}

fn build_minimal_sm4_program_chunk(program_type: u16) -> Vec<u8> {
    // SM4+ version token layout:
    // - bits 0..=3: minor version
    // - bits 4..=7: major version
    // - bits 16..=31: program type (0=ps, 1=vs, 2=gs, 3=hs, 4=ds, ...)
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

fn push_bind_shaders_ex(stream: &mut Vec<u8>, hs: u32, ds: u32) {
    // `aerogpu_cmd_bind_shaders` extended ABI:
    // - Appends `{gs, hs, ds}` handles after the legacy payload.
    let start = begin_cmd(stream, AerogpuCmdOpcode::BindShaders as u32);
    stream.extend_from_slice(&0u32.to_le_bytes()); // vs
    stream.extend_from_slice(&0u32.to_le_bytes()); // ps
    stream.extend_from_slice(&0u32.to_le_bytes()); // cs
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    stream.extend_from_slice(&0u32.to_le_bytes()); // gs
    stream.extend_from_slice(&hs.to_le_bytes());
    stream.extend_from_slice(&ds.to_le_bytes());
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

        let hs_dxbc = build_dxbc(&[(*b"SHEX", build_minimal_sm4_program_chunk(3))]);
        let ds_dxbc = build_dxbc(&[(*b"SHEX", build_minimal_sm4_program_chunk(4))]);

        let mut stream = vec![0u8; AerogpuCmdStreamHeader::SIZE_BYTES];
        stream[0..4].copy_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream[4..8].copy_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());

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

        // Bind the HS/DS via the extended BIND_SHADERS ABI.
        push_bind_shaders_ex(&mut stream, HS_SHADER, DS_SHADER);

        // VS/PS bindings (baseline routing should remain unchanged).
        push_set_constant_buffer(
            &mut stream,
            AerogpuShaderStage::Vertex as u32,
            1,
            101,
            0,
        );
        push_set_samplers(
            &mut stream,
            AerogpuShaderStage::Vertex as u32,
            0,
            201,
            0,
        );
        push_set_texture(
            &mut stream,
            AerogpuShaderStage::Vertex as u32,
            0,
            301,
            0,
        );

        push_set_constant_buffer(
            &mut stream,
            AerogpuShaderStage::Pixel as u32,
            1,
            102,
            0,
        );
        push_set_samplers(
            &mut stream,
            AerogpuShaderStage::Pixel as u32,
            0,
            202,
            0,
        );
        push_set_texture(
            &mut stream,
            AerogpuShaderStage::Pixel as u32,
            0,
            302,
            0,
        );

        // CS bindings (reserved0==0 is the real compute stage).
        push_set_constant_buffer(
            &mut stream,
            AerogpuShaderStage::Compute as u32,
            1,
            103,
            0,
        );
        push_set_samplers(
            &mut stream,
            AerogpuShaderStage::Compute as u32,
            0,
            203,
            0,
        );
        push_set_texture(
            &mut stream,
            AerogpuShaderStage::Compute as u32,
            0,
            303,
            0,
        );

        // HS bindings use shader_stage==COMPUTE + stage_ex reserved0 to select the HS bucket.
        push_set_constant_buffer(
            &mut stream,
            AerogpuShaderStage::Compute as u32,
            1,
            104,
            STAGE_EX_HULL,
        );
        push_set_samplers(
            &mut stream,
            AerogpuShaderStage::Compute as u32,
            0,
            204,
            STAGE_EX_HULL,
        );
        push_set_texture(
            &mut stream,
            AerogpuShaderStage::Compute as u32,
            0,
            304,
            STAGE_EX_HULL,
        );

        // DS bindings use shader_stage==COMPUTE + stage_ex reserved0 to select the DS bucket.
        push_set_constant_buffer(
            &mut stream,
            AerogpuShaderStage::Compute as u32,
            1,
            105,
            STAGE_EX_DOMAIN,
        );
        push_set_samplers(
            &mut stream,
            AerogpuShaderStage::Compute as u32,
            0,
            205,
            STAGE_EX_DOMAIN,
        );
        push_set_texture(
            &mut stream,
            AerogpuShaderStage::Compute as u32,
            0,
            305,
            STAGE_EX_DOMAIN,
        );

        // Second CS update ensures CS/HS/DS buckets remain distinct in both directions.
        push_set_constant_buffer(
            &mut stream,
            AerogpuShaderStage::Compute as u32,
            1,
            106,
            0,
        );
        push_set_samplers(
            &mut stream,
            AerogpuShaderStage::Compute as u32,
            0,
            206,
            0,
        );
        push_set_texture(
            &mut stream,
            AerogpuShaderStage::Compute as u32,
            0,
            306,
            0,
        );

        let stream = finish_stream(stream);

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

