mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuBlendFactor, AerogpuBlendOp, AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AerogpuPrimitiveTopology,
    AEROGPU_CLEAR_COLOR, AEROGPU_CMD_STREAM_MAGIC, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
    AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32};

const DXBC_VS_PASSTHROUGH_TEXCOORD: &[u8] = include_bytes!("fixtures/vs_passthrough_texcoord.dxbc");
const DXBC_PS_SAMPLE: &[u8] = include_bytes!("fixtures/ps_sample.dxbc");
const ILAY_POS3_TEX2: &[u8] = include_bytes!("fixtures/ilay_pos3_tex2.bin");

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(ProtocolCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(ProtocolCmdHdr, size_bytes);

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

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VertexPos3Tex2 {
    pos: [f32; 3],
    uv: [f32; 2],
}

#[test]
fn aerogpu_cmd_reuses_pipeline_layout_across_pipeline_misses() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        // Use shaders that contain reflection bindings (PS samples a texture + sampler), so the
        // pipeline layout key is non-empty. Then force a *pipeline* miss by changing blend state
        // between draws while keeping shader bindings identical; the pipeline layout should be
        // reused via the PipelineLayoutKey cache.
        const VB: u32 = 1;
        const RT: u32 = 2;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;

        let vertices = [
            VertexPos3Tex2 {
                pos: [-1.0, -1.0, 0.0],
                uv: [0.0, 0.0],
            },
            VertexPos3Tex2 {
                pos: [-1.0, 3.0, 0.0],
                uv: [0.0, 2.0],
            },
            VertexPos3Tex2 {
                pos: [3.0, -1.0, 0.0],
                uv: [2.0, 0.0],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);

        let mut stream = Vec::new();
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER.to_le_bytes());
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE (VB)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&VB.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(vb_bytes);
        stream.resize(stream.len() + (align4(vb_bytes.len()) - vb_bytes.len()), 0);
        end_cmd(&mut stream, start);

        // CREATE_TEXTURE2D (RT)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
        stream.extend_from_slice(&RT.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_RENDER_TARGET.to_le_bytes());
        stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
        stream.extend_from_slice(&4u32.to_le_bytes()); // width
        stream.extend_from_slice(&4u32.to_le_bytes()); // height
        stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
        stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
        stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (VS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // stage = vertex
        stream.extend_from_slice(&(DXBC_VS_PASSTHROUGH_TEXCOORD.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(DXBC_VS_PASSTHROUGH_TEXCOORD);
        stream.resize(
            stream.len()
                + (align4(DXBC_VS_PASSTHROUGH_TEXCOORD.len())
                    - DXBC_VS_PASSTHROUGH_TEXCOORD.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // CREATE_SHADER_DXBC (PS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&1u32.to_le_bytes()); // stage = pixel
        stream.extend_from_slice(&(DXBC_PS_SAMPLE.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(DXBC_PS_SAMPLE);
        stream.resize(
            stream.len() + (align4(DXBC_PS_SAMPLE.len()) - DXBC_PS_SAMPLE.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // CREATE_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&(ILAY_POS3_TEX2.len() as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(ILAY_POS3_TEX2);
        stream.resize(
            stream.len() + (align4(ILAY_POS3_TEX2.len()) - ILAY_POS3_TEX2.len()),
            0,
        );
        end_cmd(&mut stream, start);

        // SET_RENDER_TARGETS (1 RT, no DS)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
        stream.extend_from_slice(&RT.to_le_bytes());
        for _ in 0..7 {
            stream.extend_from_slice(&0u32.to_le_bytes());
        }
        end_cmd(&mut stream, start);

        // SET_VIEWPORT 0..4
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&4f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes());
        stream.extend_from_slice(&1f32.to_bits().to_le_bytes());
        end_cmd(&mut stream, start);

        // BIND_SHADERS
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::BindShaders as u32);
        stream.extend_from_slice(&VS.to_le_bytes());
        stream.extend_from_slice(&PS.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // cs
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_INPUT_LAYOUT
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetInputLayout as u32);
        stream.extend_from_slice(&IL.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_VERTEX_BUFFERS (slot 0)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetVertexBuffers as u32);
        stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
        stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
        stream.extend_from_slice(&VB.to_le_bytes()); // buffer
        stream.extend_from_slice(&(core::mem::size_of::<VertexPos3Tex2>() as u32).to_le_bytes()); // stride_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // SET_PRIMITIVE_TOPOLOGY
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetPrimitiveTopology as u32);
        stream.extend_from_slice(&(AerogpuPrimitiveTopology::TriangleList as u32).to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // CLEAR to a known color to avoid reading uninitialized attachments in case the triangle
        // doesn't cover the chosen readback pixel on some backend.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
        stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
        for bits in [0.0f32, 0.0, 0.0, 1.0].map(f32::to_bits) {
            stream.extend_from_slice(&bits.to_le_bytes());
        }
        stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // depth
        stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
        end_cmd(&mut stream, start);

        // DRAW (blend disabled)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        // SET_BLEND_STATE (enable blending with src=1, dst=0).
        // This forces a new render pipeline key while keeping PipelineLayoutKey identical.
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetBlendState as u32);
        stream.extend_from_slice(&1u32.to_le_bytes()); // enable
        stream.extend_from_slice(&(AerogpuBlendFactor::One as u32).to_le_bytes());
        stream.extend_from_slice(&(AerogpuBlendFactor::Zero as u32).to_le_bytes());
        stream.extend_from_slice(&(AerogpuBlendOp::Add as u32).to_le_bytes());
        stream.extend_from_slice(&0xFu32.to_le_bytes()); // write mask + padding
        end_cmd(&mut stream, start);

        // DRAW (blend enabled, but factors keep the same output)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
        stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
        stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
        stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
        end_cmd(&mut stream, start);

        let stream = finish_stream(stream);

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");

        let stats = exec.cache_stats();
        assert_eq!(stats.pipeline_layouts.misses, 1);
        assert_eq!(stats.pipeline_layouts.hits, 1);
        assert_eq!(stats.pipeline_layouts.entries, 1);
    });
}

