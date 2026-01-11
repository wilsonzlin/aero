mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::{GuestMemory, VecGuestMemory};
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AerogpuPrimitiveTopology,
    AEROGPU_CLEAR_COLOR, AEROGPU_CMD_STREAM_MAGIC, AEROGPU_COPY_FLAG_WRITEBACK_DST,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_TEXTURE,
    AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32};
use aero_protocol::aerogpu::aerogpu_ring::AerogpuAllocEntry;

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(ProtocolCmdStreamHeader, size_bytes);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(ProtocolCmdHdr, size_bytes);

fn begin_cmd(stream: &mut Vec<u8>, opcode: u32) -> usize {
    let start = stream.len();
    stream.extend_from_slice(&opcode.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
    start
}

fn end_cmd(stream: &mut Vec<u8>, start: usize) {
    let size = (stream.len() - start) as u32;
    stream[start + CMD_HDR_SIZE_BYTES_OFFSET..start + CMD_HDR_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size.to_le_bytes());
    assert_eq!(size % 4, 0, "command not 4-byte aligned");
}

fn end_stream(stream: &mut Vec<u8>) {
    let size_bytes = stream.len() as u32;
    stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size_bytes.to_le_bytes());
}

fn align4(len: usize) -> usize {
    (len + 3) & !3
}

#[test]
fn aerogpu_cmd_present_accepts_trailing_bytes() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let mut guest_mem = VecGuestMemory::new(0);

        let build_stream = |with_trailing: bool| {
            let mut stream = Vec::new();
            stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
            stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
            stream.extend_from_slice(&0u32.to_le_bytes()); // flags
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Present as u32);
            stream.extend_from_slice(&1u32.to_le_bytes()); // scanout_id
            stream.extend_from_slice(&0u32.to_le_bytes()); // flags
            if with_trailing {
                // Forward-compatible extension padding.
                stream.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
            }
            end_cmd(&mut stream, start);

            end_stream(&mut stream);
            stream
        };

        let report_base = exec
            .execute_cmd_stream(&build_stream(false), None, &mut guest_mem)
            .unwrap();
        let report_extended = exec
            .execute_cmd_stream(&build_stream(true), None, &mut guest_mem)
            .unwrap();

        assert_eq!(report_base.presents.len(), 1);
        assert_eq!(report_extended.presents.len(), 1);
        assert_eq!(report_base.presents[0].scanout_id, 1);
        assert_eq!(report_extended.presents[0].scanout_id, 1);
        assert_eq!(report_base.presents[0].flags, 0);
        assert_eq!(report_extended.presents[0].flags, 0);
    });
}

#[test]
fn aerogpu_cmd_copy_buffer_accepts_trailing_bytes() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const SRC: u32 = 1;
        const DST: u32 = 2;
        const SRC_OFFSET: u32 = 0;
        const DST_OFFSET: u32 = 0x100;
        const SIZE_BYTES: u64 = 16;

        let alloc = AerogpuAllocEntry {
            alloc_id: 1,
            flags: 0,
            gpa: 0x100,
            size_bytes: 0x1000,
            reserved0: 0,
        };
        let allocs = [alloc];

        let src_bytes: [u8; SIZE_BYTES as usize] = *b"hello aero-gpu!!";

        let build_stream = |with_trailing: bool| {
            let mut stream = Vec::new();
            stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
            stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
            stream.extend_from_slice(&0u32.to_le_bytes()); // flags
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

            // CREATE_BUFFER SRC
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
            stream.extend_from_slice(&SRC.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
            stream.extend_from_slice(&SIZE_BYTES.to_le_bytes());
            stream.extend_from_slice(&alloc.alloc_id.to_le_bytes());
            stream.extend_from_slice(&SRC_OFFSET.to_le_bytes());
            stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);

            // CREATE_BUFFER DST
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
            stream.extend_from_slice(&DST.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
            stream.extend_from_slice(&SIZE_BYTES.to_le_bytes());
            stream.extend_from_slice(&alloc.alloc_id.to_le_bytes());
            stream.extend_from_slice(&DST_OFFSET.to_le_bytes());
            stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);

            // COPY_BUFFER with WRITEBACK_DST.
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyBuffer as u32);
            stream.extend_from_slice(&DST.to_le_bytes()); // dst_buffer
            stream.extend_from_slice(&SRC.to_le_bytes()); // src_buffer
            stream.extend_from_slice(&0u64.to_le_bytes()); // dst_offset_bytes
            stream.extend_from_slice(&0u64.to_le_bytes()); // src_offset_bytes
            stream.extend_from_slice(&SIZE_BYTES.to_le_bytes());
            stream.extend_from_slice(&AEROGPU_COPY_FLAG_WRITEBACK_DST.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            if with_trailing {
                // Forward-compatible extension padding.
                stream.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
            }
            end_cmd(&mut stream, start);

            end_stream(&mut stream);
            stream
        };

        let run = |exec: &mut AerogpuD3d11Executor, with_trailing: bool| -> VecGuestMemory {
            let mut guest_mem = VecGuestMemory::new(0x2000);
            guest_mem
                .write(alloc.gpa + SRC_OFFSET as u64, &src_bytes)
                .unwrap();
            guest_mem
                .write(alloc.gpa + DST_OFFSET as u64, &[0u8; SIZE_BYTES as usize])
                .unwrap();

            exec.execute_cmd_stream(&build_stream(with_trailing), Some(&allocs), &mut guest_mem)
                .unwrap();
            exec.poll_wait();
            guest_mem
        };

        let mut guest_mem_base = run(&mut exec, false);
        let mut out = [0u8; SIZE_BYTES as usize];
        guest_mem_base
            .read(alloc.gpa + DST_OFFSET as u64, &mut out)
            .unwrap();
        assert_eq!(out, src_bytes);

        exec.reset();

        let mut guest_mem_extended = run(&mut exec, true);
        let mut out = [0u8; SIZE_BYTES as usize];
        guest_mem_extended
            .read(alloc.gpa + DST_OFFSET as u64, &mut out)
            .unwrap();
        assert_eq!(out, src_bytes);
    });
}

#[test]
fn aerogpu_cmd_copy_texture2d_accepts_trailing_bytes() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const SRC: u32 = 1;
        const DST: u32 = 2;
        const SRC_OFFSET: u32 = 0;
        const DST_OFFSET: u32 = 0x100;
        const WIDTH: u32 = 2;
        const HEIGHT: u32 = 2;
        const ROW_PITCH: u32 = WIDTH * 4;
        const TEX_SIZE: usize = (ROW_PITCH as usize) * (HEIGHT as usize);

        let alloc = AerogpuAllocEntry {
            alloc_id: 1,
            flags: 0,
            gpa: 0x100,
            size_bytes: 0x1000,
            reserved0: 0,
        };
        let allocs = [alloc];

        let src_pixels: [u8; TEX_SIZE] = [
            0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80, 0x01, 0x02, 0x03, 0x04, 0xAA, 0xBB,
            0xCC, 0xDD,
        ];

        let build_stream = |with_trailing: bool| {
            let mut stream = Vec::new();
            stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
            stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
            stream.extend_from_slice(&0u32.to_le_bytes()); // flags
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

            for (handle, offset) in [(SRC, SRC_OFFSET), (DST, DST_OFFSET)] {
                let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
                stream.extend_from_slice(&handle.to_le_bytes());
                stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
                stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
                stream.extend_from_slice(&WIDTH.to_le_bytes());
                stream.extend_from_slice(&HEIGHT.to_le_bytes());
                stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
                stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
                stream.extend_from_slice(&ROW_PITCH.to_le_bytes());
                stream.extend_from_slice(&alloc.alloc_id.to_le_bytes());
                stream.extend_from_slice(&offset.to_le_bytes());
                stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
                end_cmd(&mut stream, start);
            }

            // COPY_TEXTURE2D with WRITEBACK_DST.
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyTexture2d as u32);
            stream.extend_from_slice(&DST.to_le_bytes());
            stream.extend_from_slice(&SRC.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // dst_mip_level
            stream.extend_from_slice(&0u32.to_le_bytes()); // dst_array_layer
            stream.extend_from_slice(&0u32.to_le_bytes()); // src_mip_level
            stream.extend_from_slice(&0u32.to_le_bytes()); // src_array_layer
            stream.extend_from_slice(&0u32.to_le_bytes()); // dst_x
            stream.extend_from_slice(&0u32.to_le_bytes()); // dst_y
            stream.extend_from_slice(&0u32.to_le_bytes()); // src_x
            stream.extend_from_slice(&0u32.to_le_bytes()); // src_y
            stream.extend_from_slice(&WIDTH.to_le_bytes());
            stream.extend_from_slice(&HEIGHT.to_le_bytes());
            stream.extend_from_slice(&AEROGPU_COPY_FLAG_WRITEBACK_DST.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            if with_trailing {
                // Forward-compatible extension padding.
                stream.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
            }
            end_cmd(&mut stream, start);

            end_stream(&mut stream);
            stream
        };

        let run = |exec: &mut AerogpuD3d11Executor, with_trailing: bool| -> VecGuestMemory {
            let mut guest_mem = VecGuestMemory::new(0x2000);
            guest_mem
                .write(alloc.gpa + SRC_OFFSET as u64, &src_pixels)
                .unwrap();
            guest_mem
                .write(alloc.gpa + DST_OFFSET as u64, &[0u8; TEX_SIZE])
                .unwrap();

            exec.execute_cmd_stream(&build_stream(with_trailing), Some(&allocs), &mut guest_mem)
                .unwrap();
            exec.poll_wait();
            guest_mem
        };

        let mut guest_mem_base = run(&mut exec, false);
        let mut out = [0u8; TEX_SIZE];
        guest_mem_base
            .read(alloc.gpa + DST_OFFSET as u64, &mut out)
            .unwrap();
        assert_eq!(out, src_pixels);

        exec.reset();

        let mut guest_mem_extended = run(&mut exec, true);
        let mut out = [0u8; TEX_SIZE];
        guest_mem_extended
            .read(alloc.gpa + DST_OFFSET as u64, &mut out)
            .unwrap();
        assert_eq!(out, src_pixels);
    });
}

#[test]
fn aerogpu_cmd_set_samplers_accepts_trailing_bytes() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let mut guest_mem = VecGuestMemory::new(0);

        let build_stream = |with_trailing: bool| {
            let mut stream = Vec::new();
            stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
            stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
            stream.extend_from_slice(&0u32.to_le_bytes()); // flags
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

            // SET_SAMPLERS (PS s0..s1)
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetSamplers as u32);
            stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
            stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
            stream.extend_from_slice(&2u32.to_le_bytes()); // sampler_count
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            stream.extend_from_slice(&123u32.to_le_bytes()); // samplers[0]
            stream.extend_from_slice(&0u32.to_le_bytes()); // samplers[1] = unbind
            if with_trailing {
                // Forward-compatible extension padding.
                stream.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
            }
            end_cmd(&mut stream, start);

            end_stream(&mut stream);
            stream
        };

        exec.execute_cmd_stream(&build_stream(false), None, &mut guest_mem)
            .unwrap();
        exec.execute_cmd_stream(&build_stream(true), None, &mut guest_mem)
            .unwrap();
    });
}

#[test]
fn aerogpu_cmd_set_constant_buffers_accepts_trailing_bytes() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let mut guest_mem = VecGuestMemory::new(0);

        let build_stream = |with_trailing: bool| {
            let mut stream = Vec::new();
            stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
            stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
            stream.extend_from_slice(&0u32.to_le_bytes()); // flags
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

            // SET_CONSTANT_BUFFERS (VS b0)
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetConstantBuffers as u32);
            stream.extend_from_slice(&0u32.to_le_bytes()); // shader_stage = vertex
            stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
            stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
                                                           // bindings[0]
            stream.extend_from_slice(&0u32.to_le_bytes()); // buffer = unbound
            stream.extend_from_slice(&0u32.to_le_bytes()); // offset_bytes
            stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            if with_trailing {
                // Forward-compatible extension padding.
                stream.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
            }
            end_cmd(&mut stream, start);

            end_stream(&mut stream);
            stream
        };

        exec.execute_cmd_stream(&build_stream(false), None, &mut guest_mem)
            .unwrap();
        exec.execute_cmd_stream(&build_stream(true), None, &mut guest_mem)
            .unwrap();
    });
}

#[test]
fn aerogpu_cmd_set_shader_constants_f_accepts_trailing_bytes() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let mut guest_mem = VecGuestMemory::new(0);

        let build_stream = |with_trailing: bool| {
            let mut stream = Vec::new();
            stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
            stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
            stream.extend_from_slice(&0u32.to_le_bytes()); // flags
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

            // SET_SHADER_CONSTANTS_F (VS c0)
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetShaderConstantsF as u32);
            stream.extend_from_slice(&0u32.to_le_bytes()); // shader_stage = vertex
            stream.extend_from_slice(&0u32.to_le_bytes()); // start_register
            stream.extend_from_slice(&1u32.to_le_bytes()); // vec4_count
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            for f in [1.0f32, 2.0, 3.0, 4.0] {
                stream.extend_from_slice(&f.to_bits().to_le_bytes());
            }
            if with_trailing {
                // Forward-compatible extension padding.
                stream.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
            }
            end_cmd(&mut stream, start);

            end_stream(&mut stream);
            stream
        };

        exec.execute_cmd_stream(&build_stream(false), None, &mut guest_mem)
            .unwrap();
        exec.poll_wait();
        exec.reset();
        exec.execute_cmd_stream(&build_stream(true), None, &mut guest_mem)
            .unwrap();
        exec.poll_wait();
    });
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct TexVertex {
    pos: [f32; 3],
    uv: [f32; 2],
}

#[test]
fn aerogpu_cmd_sampler_and_texture_bindings_accept_trailing_bytes() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const VB: u32 = 1;
        const TEX: u32 = 2;
        const RT: u32 = 3;
        const VS: u32 = 10;
        const PS: u32 = 11;
        const IL: u32 = 20;
        const SAMP: u32 = 30;

        let vertices = [
            TexVertex {
                pos: [-1.0, -1.0, 0.0],
                uv: [0.0, 1.0],
            },
            TexVertex {
                pos: [-1.0, 3.0, 0.0],
                uv: [0.0, -1.0],
            },
            TexVertex {
                pos: [3.0, -1.0, 0.0],
                uv: [2.0, 1.0],
            },
        ];
        let vb_bytes = bytemuck::bytes_of(&vertices);

        // 2x2 RGBA pattern (top-left origin):
        //   row0: red, green
        //   row1: blue, white
        let tex_bytes: [u8; 16] = [
            255, 0, 0, 255, // red
            0, 255, 0, 255, // green
            0, 0, 255, 255, // blue
            255, 255, 255, 255, // white
        ];

        let vs_dxbc = include_bytes!("fixtures/vs_passthrough_texcoord.dxbc");
        let ps_dxbc = include_bytes!("fixtures/ps_sample.dxbc");
        let ilay = include_bytes!("fixtures/ilay_pos3_tex2.bin");

        let build_stream = |with_trailing: bool| {
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
            stream.extend_from_slice(&0u64.to_le_bytes()); // offset
            stream.extend_from_slice(&(vb_bytes.len() as u64).to_le_bytes());
            stream.extend_from_slice(vb_bytes);
            stream.resize(stream.len() + (align4(vb_bytes.len()) - vb_bytes.len()), 0);
            end_cmd(&mut stream, start);

            // CREATE_TEXTURE2D (TEX)
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
            stream.extend_from_slice(&TEX.to_le_bytes());
            stream.extend_from_slice(&AEROGPU_RESOURCE_USAGE_TEXTURE.to_le_bytes());
            stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
            stream.extend_from_slice(&2u32.to_le_bytes()); // width
            stream.extend_from_slice(&2u32.to_le_bytes()); // height
            stream.extend_from_slice(&1u32.to_le_bytes()); // mip_levels
            stream.extend_from_slice(&1u32.to_le_bytes()); // array_layers
            stream.extend_from_slice(&0u32.to_le_bytes()); // row_pitch_bytes
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
            stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);

            // UPLOAD_RESOURCE (TEX)
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
            stream.extend_from_slice(&TEX.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            stream.extend_from_slice(&0u64.to_le_bytes()); // offset
            stream.extend_from_slice(&(tex_bytes.len() as u64).to_le_bytes());
            stream.extend_from_slice(&tex_bytes);
            stream.resize(
                stream.len() + (align4(tex_bytes.len()) - tex_bytes.len()),
                0,
            );
            end_cmd(&mut stream, start);

            // CREATE_TEXTURE2D (RT)
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateTexture2d as u32);
            stream.extend_from_slice(&RT.to_le_bytes());
            stream.extend_from_slice(
                &(AEROGPU_RESOURCE_USAGE_TEXTURE | AEROGPU_RESOURCE_USAGE_RENDER_TARGET)
                    .to_le_bytes(),
            );
            stream.extend_from_slice(&(AerogpuFormat::R8G8B8A8Unorm as u32).to_le_bytes());
            stream.extend_from_slice(&64u32.to_le_bytes()); // width
            stream.extend_from_slice(&64u32.to_le_bytes()); // height
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
            stream.extend_from_slice(&(vs_dxbc.len() as u32).to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            stream.extend_from_slice(vs_dxbc);
            stream.resize(stream.len() + (align4(vs_dxbc.len()) - vs_dxbc.len()), 0);
            end_cmd(&mut stream, start);

            // CREATE_SHADER_DXBC (PS)
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateShaderDxbc as u32);
            stream.extend_from_slice(&PS.to_le_bytes());
            stream.extend_from_slice(&1u32.to_le_bytes()); // stage = pixel
            stream.extend_from_slice(&(ps_dxbc.len() as u32).to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            stream.extend_from_slice(ps_dxbc);
            stream.resize(stream.len() + (align4(ps_dxbc.len()) - ps_dxbc.len()), 0);
            end_cmd(&mut stream, start);

            // BIND_SHADERS
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::BindShaders as u32);
            stream.extend_from_slice(&VS.to_le_bytes());
            stream.extend_from_slice(&PS.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // cs
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);

            // CREATE_INPUT_LAYOUT
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateInputLayout as u32);
            stream.extend_from_slice(&IL.to_le_bytes());
            stream.extend_from_slice(&(ilay.len() as u32).to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            stream.extend_from_slice(ilay);
            stream.resize(stream.len() + (align4(ilay.len()) - ilay.len()), 0);
            end_cmd(&mut stream, start);

            // SET_INPUT_LAYOUT
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetInputLayout as u32);
            stream.extend_from_slice(&IL.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);

            // CREATE_SAMPLER (nearest + clamp)
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateSampler as u32);
            stream.extend_from_slice(&SAMP.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // filter = nearest
            stream.extend_from_slice(&0u32.to_le_bytes()); // address_u = clamp
            stream.extend_from_slice(&0u32.to_le_bytes()); // address_v = clamp
            stream.extend_from_slice(&0u32.to_le_bytes()); // address_w = clamp
            if with_trailing {
                stream.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
            }
            end_cmd(&mut stream, start);

            // SET_SAMPLER_STATE (ignored by executor, but should accept forward-compat padding)
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetSamplerState as u32);
            stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
            stream.extend_from_slice(&0u32.to_le_bytes()); // slot
            stream.extend_from_slice(&0u32.to_le_bytes()); // state
            stream.extend_from_slice(&0u32.to_le_bytes()); // value
            if with_trailing {
                stream.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
            }
            end_cmd(&mut stream, start);

            // SET_TEXTURE (PS t0)
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetTexture as u32);
            stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
            stream.extend_from_slice(&0u32.to_le_bytes()); // slot
            stream.extend_from_slice(&TEX.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            if with_trailing {
                stream.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
            }
            end_cmd(&mut stream, start);

            // SET_SAMPLERS (PS s0)
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetSamplers as u32);
            stream.extend_from_slice(&1u32.to_le_bytes()); // shader_stage = pixel
            stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
            stream.extend_from_slice(&1u32.to_le_bytes()); // sampler_count
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            stream.extend_from_slice(&SAMP.to_le_bytes()); // samplers[0]
            if with_trailing {
                stream.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
            }
            end_cmd(&mut stream, start);

            // SET_RENDER_TARGETS
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetRenderTargets as u32);
            stream.extend_from_slice(&1u32.to_le_bytes()); // color_count
            stream.extend_from_slice(&0u32.to_le_bytes()); // depth_stencil
            stream.extend_from_slice(&RT.to_le_bytes()); // rt0
            for _ in 1..8 {
                stream.extend_from_slice(&0u32.to_le_bytes());
            }
            end_cmd(&mut stream, start);

            // SET_VIEWPORT
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetViewport as u32);
            stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // x
            stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // y
            stream.extend_from_slice(&64f32.to_bits().to_le_bytes()); // w
            stream.extend_from_slice(&64f32.to_bits().to_le_bytes()); // h
            stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // min_depth
            stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // max_depth
            end_cmd(&mut stream, start);

            // SET_SCISSOR
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetScissor as u32);
            stream.extend_from_slice(&0i32.to_le_bytes()); // x
            stream.extend_from_slice(&0i32.to_le_bytes()); // y
            stream.extend_from_slice(&64i32.to_le_bytes()); // w
            stream.extend_from_slice(&64i32.to_le_bytes()); // h
            end_cmd(&mut stream, start);

            // SET_VERTEX_BUFFERS
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetVertexBuffers as u32);
            stream.extend_from_slice(&0u32.to_le_bytes()); // start_slot
            stream.extend_from_slice(&1u32.to_le_bytes()); // buffer_count
            stream.extend_from_slice(&VB.to_le_bytes());
            stream.extend_from_slice(&(std::mem::size_of::<TexVertex>() as u32).to_le_bytes()); // stride
            stream.extend_from_slice(&0u32.to_le_bytes()); // offset
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);

            // SET_PRIMITIVE_TOPOLOGY
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::SetPrimitiveTopology as u32);
            stream
                .extend_from_slice(&(AerogpuPrimitiveTopology::TriangleList as u32).to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);

            // CLEAR (black)
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Clear as u32);
            stream.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
            stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // r
            stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // g
            stream.extend_from_slice(&0f32.to_bits().to_le_bytes()); // b
            stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // a
            stream.extend_from_slice(&1f32.to_bits().to_le_bytes()); // depth
            stream.extend_from_slice(&0u32.to_le_bytes()); // stencil
            end_cmd(&mut stream, start);

            // DRAW
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Draw as u32);
            stream.extend_from_slice(&3u32.to_le_bytes()); // vertex_count
            stream.extend_from_slice(&1u32.to_le_bytes()); // instance_count
            stream.extend_from_slice(&0u32.to_le_bytes()); // first_vertex
            stream.extend_from_slice(&0u32.to_le_bytes()); // first_instance
            end_cmd(&mut stream, start);

            // PRESENT
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::Present as u32);
            stream.extend_from_slice(&0u32.to_le_bytes()); // scanout_id
            stream.extend_from_slice(&0u32.to_le_bytes()); // flags
            end_cmd(&mut stream, start);

            // DESTROY_SAMPLER
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::DestroySampler as u32);
            stream.extend_from_slice(&SAMP.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
            if with_trailing {
                stream.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
            }
            end_cmd(&mut stream, start);

            end_stream(&mut stream);
            stream
        };

        let pixels_base = {
            let mut guest_mem = VecGuestMemory::new(0x1000);
            exec.execute_cmd_stream(&build_stream(false), None, &mut guest_mem)
                .unwrap();
            exec.poll_wait();
            exec.read_texture_rgba8(RT).await.unwrap()
        };
        assert_eq!(pixels_base.len(), 64 * 64 * 4);

        let sample = |pixels: &[u8], x: usize, y: usize| -> [u8; 4] {
            let off = (y * 64 + x) * 4;
            pixels[off..off + 4].try_into().unwrap()
        };

        assert_eq!(sample(&pixels_base, 16, 16), [255, 0, 0, 255]);
        assert_eq!(sample(&pixels_base, 48, 16), [0, 255, 0, 255]);
        assert_eq!(sample(&pixels_base, 16, 48), [0, 0, 255, 255]);
        assert_eq!(sample(&pixels_base, 48, 48), [255, 255, 255, 255]);

        exec.reset();

        let pixels_ext = {
            let mut guest_mem = VecGuestMemory::new(0x1000);
            exec.execute_cmd_stream(&build_stream(true), None, &mut guest_mem)
                .unwrap();
            exec.poll_wait();
            exec.read_texture_rgba8(RT).await.unwrap()
        };
        assert_eq!(pixels_ext.len(), 64 * 64 * 4);
        assert_eq!(sample(&pixels_ext, 16, 16), [255, 0, 0, 255]);
        assert_eq!(sample(&pixels_ext, 48, 16), [0, 255, 0, 255]);
        assert_eq!(sample(&pixels_ext, 16, 48), [0, 0, 255, 255]);
        assert_eq!(sample(&pixels_ext, 48, 48), [255, 255, 255, 255]);
    });
}
