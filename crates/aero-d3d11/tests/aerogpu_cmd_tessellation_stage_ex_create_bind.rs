mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::runtime::bindings::{BoundConstantBuffer, BoundTexture, ShaderStage};
use aero_d3d11::FourCC;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode, AerogpuCmdStreamHeader, AerogpuShaderStage,
    AEROGPU_CMD_STREAM_MAGIC,
};
use aero_protocol::aerogpu::aerogpu_pci::AEROGPU_ABI_VERSION_U32;

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(AerogpuCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(ProtocolCmdHdr, size_bytes);

// `stage_ex` values use DXBC program-type numbering (SM4/SM5 version token).
const STAGE_EX_HULL: u32 = 3;
const STAGE_EX_DOMAIN: u32 = 4;

const FOURCC_SHEX: FourCC = FourCC(*b"SHEX");

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
        bytes.extend_from_slice(&fourcc.0);
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

fn push_bind_shaders_ex(
    stream: &mut Vec<u8>,
    vs: u32,
    ps: u32,
    cs: u32,
    gs: u32,
    hs: u32,
    ds: u32,
) {
    // Extended payload appends `{gs, hs, ds}` after the legacy `{vs, ps, cs, reserved0}`.
    let start = begin_cmd(stream, AerogpuCmdOpcode::BindShaders as u32);
    stream.extend_from_slice(&vs.to_le_bytes());
    stream.extend_from_slice(&ps.to_le_bytes());
    stream.extend_from_slice(&cs.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0 (legacy GS handle) left zero.
    stream.extend_from_slice(&gs.to_le_bytes());
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
fn aerogpu_cmd_create_and_bind_hs_ds_stage_ex() {
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
        const CS_SHADER: u32 = 0xDEAD_BEEF;

        let hs_dxbc = build_dxbc(&[(FOURCC_SHEX, build_minimal_sm4_program_chunk(3))]);
        let ds_dxbc = build_dxbc(&[(FOURCC_SHEX, build_minimal_sm4_program_chunk(4))]);

        let mut stream = vec![0u8; AerogpuCmdStreamHeader::SIZE_BYTES];
        stream[0..4].copy_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream[4..8].copy_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());

        // CREATE_SHADER_DXBC (HS).
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

        // CREATE_SHADER_DXBC (DS).
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

        // Bind HS/DS via the extended BIND_SHADERS payload (`{gs, hs, ds}`).
        push_bind_shaders_ex(&mut stream, 0, 0, CS_SHADER, 0, HS_SHADER, DS_SHADER);

        // Set baseline CS bindings.
        push_set_constant_buffer(&mut stream, AerogpuShaderStage::Compute as u32, 1, 101, 0);
        push_set_texture(&mut stream, AerogpuShaderStage::Compute as u32, 0, 201, 0);

        // HS/DS binding updates must not overwrite CS stage state.
        push_set_constant_buffer(
            &mut stream,
            AerogpuShaderStage::Compute as u32,
            1,
            102,
            STAGE_EX_HULL,
        );
        push_set_texture(
            &mut stream,
            AerogpuShaderStage::Compute as u32,
            0,
            202,
            STAGE_EX_HULL,
        );

        push_set_constant_buffer(
            &mut stream,
            AerogpuShaderStage::Compute as u32,
            1,
            103,
            STAGE_EX_DOMAIN,
        );
        push_set_texture(
            &mut stream,
            AerogpuShaderStage::Compute as u32,
            0,
            203,
            STAGE_EX_DOMAIN,
        );

        // Second CS update ensures CS remains distinct even after HS/DS stage_ex updates.
        push_set_constant_buffer(&mut stream, AerogpuShaderStage::Compute as u32, 1, 104, 0);
        push_set_texture(&mut stream, AerogpuShaderStage::Compute as u32, 0, 204, 0);

        let stream = finish_stream(stream);

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("command stream should execute");

        // Ensure HS/DS shaders were stored and retain their stage buckets.
        assert_eq!(
            exec.shader_entry_point(HS_SHADER)
                .expect("HS shader should exist after CREATE_SHADER_DXBC"),
            "hs_main"
        );
        assert_eq!(
            exec.shader_entry_point(DS_SHADER)
                .expect("DS shader should exist after CREATE_SHADER_DXBC"),
            "ds_main"
        );
        assert_eq!(
            exec.shader_stage(HS_SHADER)
                .expect("HS shader stage should be stored"),
            ShaderStage::Hull
        );
        assert_eq!(
            exec.shader_stage(DS_SHADER)
                .expect("DS shader stage should be stored"),
            ShaderStage::Domain
        );

        // Ensure BIND_SHADERS set HS/DS and did not clobber CS.
        let bound = exec.bound_shader_handles();
        assert_eq!(bound.cs, Some(CS_SHADER));
        assert_eq!(bound.hs, Some(HS_SHADER));
        assert_eq!(bound.ds, Some(DS_SHADER));

        // Ensure stage_ex binding tables for HS/DS do not clobber CS bindings.
        let bindings = exec.binding_state();
        let expect_cb = |buffer: u32| {
            Some(BoundConstantBuffer {
                buffer,
                offset: 0,
                size: Some(16),
            })
        };

        assert_eq!(
            bindings.stage(ShaderStage::Compute).constant_buffer(1),
            expect_cb(104)
        );
        assert_eq!(
            bindings.stage(ShaderStage::Compute).texture(0),
            Some(BoundTexture { texture: 204 })
        );

        assert_eq!(
            bindings.stage(ShaderStage::Hull).constant_buffer(1),
            expect_cb(102)
        );
        assert_eq!(
            bindings.stage(ShaderStage::Hull).texture(0),
            Some(BoundTexture { texture: 202 })
        );

        assert_eq!(
            bindings.stage(ShaderStage::Domain).constant_buffer(1),
            expect_cb(103)
        );
        assert_eq!(
            bindings.stage(ShaderStage::Domain).texture(0),
            Some(BoundTexture { texture: 203 })
        );
    });
}
