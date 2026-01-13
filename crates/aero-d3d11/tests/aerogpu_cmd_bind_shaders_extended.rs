mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdOpcode, AerogpuCmdStreamHeader as ProtocolCmdStreamHeader,
};
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

#[test]
fn aerogpu_cmd_tracks_extended_bind_shaders_payload() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let mut writer = AerogpuCmdWriter::new();
        writer.bind_shaders(10, 20, 30);
        let mut stream = writer.finish();

        // Patch the first command (BIND_SHADERS) to use the extended payload:
        // hdr(8) + vs/ps/cs/reserved0 (16) + {gs,hs,ds} (12) = 36 bytes.
        let cmd_start = ProtocolCmdStreamHeader::SIZE_BYTES;
        let opcode = u32::from_le_bytes(stream[cmd_start..cmd_start + 4].try_into().unwrap());
        assert_eq!(opcode, AerogpuCmdOpcode::BindShaders as u32);
        stream[cmd_start + 4..cmd_start + 8].copy_from_slice(&(36u32).to_le_bytes());

        stream.extend_from_slice(&40u32.to_le_bytes()); // gs
        stream.extend_from_slice(&50u32.to_le_bytes()); // hs
        stream.extend_from_slice(&60u32.to_le_bytes()); // ds

        // Update stream header size_bytes.
        let total_size = stream.len() as u32;
        let size_off = core::mem::offset_of!(ProtocolCmdStreamHeader, size_bytes);
        stream[size_off..size_off + 4].copy_from_slice(&total_size.to_le_bytes());

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");

        let bound = exec.bound_shader_handles();
        assert_eq!(bound.vs, Some(10));
        assert_eq!(bound.ps, Some(20));
        assert_eq!(bound.cs, Some(30));
        assert_eq!(bound.gs, Some(40));
        assert_eq!(bound.hs, Some(50));
        assert_eq!(bound.ds, Some(60));
    });
}

