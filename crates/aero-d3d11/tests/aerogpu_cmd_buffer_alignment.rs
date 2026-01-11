mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AEROGPU_CMD_STREAM_MAGIC,
};
use aero_protocol::aerogpu::aerogpu_pci::AEROGPU_ABI_VERSION_U32;

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

fn end_cmd(stream: &mut Vec<u8>, start: usize) {
    let size = (stream.len() - start) as u32;
    stream[start + CMD_HDR_SIZE_BYTES_OFFSET..start + CMD_HDR_SIZE_BYTES_OFFSET + 4]
        .copy_from_slice(&size.to_le_bytes());
    assert_eq!(size % 4, 0, "command not 4-byte aligned");
}

#[test]
fn aerogpu_cmd_rejects_unaligned_buffer_upload() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        const BUF: u32 = 1;

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        // CREATE_BUFFER (host allocated)
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
        stream.extend_from_slice(&BUF.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
        stream.extend_from_slice(&16u64.to_le_bytes()); // size_bytes
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
        stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // UPLOAD_RESOURCE with size_bytes=3 (not COPY_BUFFER_ALIGNMENT-aligned).
        let payload = [1u8, 2, 3];
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::UploadResource as u32);
        stream.extend_from_slice(&BUF.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u64.to_le_bytes()); // offset_bytes
        stream.extend_from_slice(&(payload.len() as u64).to_le_bytes()); // size_bytes
        stream.extend_from_slice(&payload);
        stream.resize(stream.len() + (align4(payload.len()) - payload.len()), 0);
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        let guest_mem = VecGuestMemory::new(0);
        let err = exec
            .execute_cmd_stream(&stream, None, &guest_mem)
            .expect_err("expected unaligned buffer upload to be rejected");
        assert!(
            format!("{err:#}").contains("UPLOAD_RESOURCE"),
            "unexpected error: {err:#}"
        );
    });
}

#[test]
fn aerogpu_cmd_rejects_unaligned_copy_buffer() {
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

        let mut stream = Vec::new();
        // Stream header (24 bytes)
        stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
        stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        stream.extend_from_slice(&0u32.to_le_bytes()); // size_bytes (patched later)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1

        for handle in [SRC, DST] {
            let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CreateBuffer as u32);
            stream.extend_from_slice(&handle.to_le_bytes());
            stream.extend_from_slice(&0u32.to_le_bytes()); // usage_flags
            stream.extend_from_slice(&16u64.to_le_bytes()); // size_bytes
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_alloc_id
            stream.extend_from_slice(&0u32.to_le_bytes()); // backing_offset_bytes
            stream.extend_from_slice(&0u64.to_le_bytes()); // reserved0
            end_cmd(&mut stream, start);
        }

        // COPY_BUFFER with size_bytes=2 (not COPY_BUFFER_ALIGNMENT-aligned).
        let start = begin_cmd(&mut stream, AerogpuCmdOpcode::CopyBuffer as u32);
        stream.extend_from_slice(&DST.to_le_bytes()); // dst_buffer
        stream.extend_from_slice(&SRC.to_le_bytes()); // src_buffer
        stream.extend_from_slice(&0u64.to_le_bytes()); // dst_offset_bytes
        stream.extend_from_slice(&0u64.to_le_bytes()); // src_offset_bytes
        stream.extend_from_slice(&2u64.to_le_bytes()); // size_bytes (unaligned)
        stream.extend_from_slice(&0u32.to_le_bytes()); // flags
        stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
        end_cmd(&mut stream, start);

        // Patch stream size in header.
        let total_size = stream.len() as u32;
        stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
            .copy_from_slice(&total_size.to_le_bytes());

        let guest_mem = VecGuestMemory::new(0);
        let err = exec
            .execute_cmd_stream(&stream, None, &guest_mem)
            .expect_err("expected unaligned copy_buffer to be rejected");
        assert!(
            format!("{err:#}").contains("COPY_BUFFER"),
            "unexpected error: {err:#}"
        );
    });
}
