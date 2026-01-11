use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::{GuestMemory, VecGuestMemory};
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AEROGPU_CMD_STREAM_MAGIC,
    AEROGPU_COPY_FLAG_WRITEBACK_DST,
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

#[test]
fn aerogpu_cmd_present_accepts_trailing_bytes() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                eprintln!("wgpu unavailable ({e:#}); skipping aerogpu_cmd forward-compat test");
                return;
            }
        };

        let guest_mem = VecGuestMemory::new(0);

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
            .execute_cmd_stream(&build_stream(false), None, &guest_mem)
            .unwrap();
        let report_extended = exec
            .execute_cmd_stream(&build_stream(true), None, &guest_mem)
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
                eprintln!("wgpu unavailable ({e:#}); skipping aerogpu_cmd forward-compat test");
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
            let guest_mem = VecGuestMemory::new(0x2000);
            guest_mem
                .write(alloc.gpa + SRC_OFFSET as u64, &src_bytes)
                .unwrap();
            guest_mem
                .write(alloc.gpa + DST_OFFSET as u64, &[0u8; SIZE_BYTES as usize])
                .unwrap();

            exec.execute_cmd_stream(&build_stream(with_trailing), Some(&allocs), &guest_mem)
                .unwrap();
            exec.poll_wait();
            guest_mem
        };

        let guest_mem_base = run(&mut exec, false);
        let mut out = [0u8; SIZE_BYTES as usize];
        guest_mem_base
            .read(alloc.gpa + DST_OFFSET as u64, &mut out)
            .unwrap();
        assert_eq!(out, src_bytes);

        exec.reset();

        let guest_mem_extended = run(&mut exec, true);
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
                eprintln!("wgpu unavailable ({e:#}); skipping aerogpu_cmd forward-compat test");
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
            let guest_mem = VecGuestMemory::new(0x2000);
            guest_mem
                .write(alloc.gpa + SRC_OFFSET as u64, &src_pixels)
                .unwrap();
            guest_mem
                .write(alloc.gpa + DST_OFFSET as u64, &[0u8; TEX_SIZE])
                .unwrap();

            exec.execute_cmd_stream(&build_stream(with_trailing), Some(&allocs), &guest_mem)
                .unwrap();
            exec.poll_wait();
            guest_mem
        };

        let guest_mem_base = run(&mut exec, false);
        let mut out = [0u8; TEX_SIZE];
        guest_mem_base
            .read(alloc.gpa + DST_OFFSET as u64, &mut out)
            .unwrap();
        assert_eq!(out, src_pixels);

        exec.reset();

        let guest_mem_extended = run(&mut exec, true);
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
                eprintln!("wgpu unavailable ({e:#}); skipping aerogpu_cmd forward-compat test");
                return;
            }
        };

        let guest_mem = VecGuestMemory::new(0);

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

        exec.execute_cmd_stream(&build_stream(false), None, &guest_mem)
            .unwrap();
        exec.execute_cmd_stream(&build_stream(true), None, &guest_mem)
            .unwrap();
    });
}

#[test]
fn aerogpu_cmd_set_constant_buffers_accepts_trailing_bytes() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                eprintln!("wgpu unavailable ({e:#}); skipping aerogpu_cmd forward-compat test");
                return;
            }
        };

        let guest_mem = VecGuestMemory::new(0);

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

        exec.execute_cmd_stream(&build_stream(false), None, &guest_mem)
            .unwrap();
        exec.execute_cmd_stream(&build_stream(true), None, &guest_mem)
            .unwrap();
    });
}
