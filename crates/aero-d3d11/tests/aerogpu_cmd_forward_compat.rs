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

