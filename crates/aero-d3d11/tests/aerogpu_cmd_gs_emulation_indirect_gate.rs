mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode, AerogpuCmdStreamHeader as ProtocolHeader,
};

const CMD_TRIANGLE_SM4: &[u8] = include_bytes!("fixtures/cmd_triangle_sm4.bin");

fn patch_first_bind_shaders_gs(bytes: &mut [u8]) {
    let mut cursor = ProtocolHeader::SIZE_BYTES;
    let mut patched = false;

    while cursor + ProtocolCmdHdr::SIZE_BYTES <= bytes.len() {
        let opcode = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap());
        let size = u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
        if size == 0 || cursor + size > bytes.len() {
            break;
        }

        if opcode == AerogpuCmdOpcode::BindShaders as u32 {
            // struct aerogpu_cmd_bind_shaders:
            // hdr(8) + vs(4) + ps(4) + cs(4) + reserved0(4)
            assert_eq!(size, 24, "unexpected BindShaders size");
            let vs = u32::from_le_bytes(bytes[cursor + 8..cursor + 12].try_into().unwrap());
            // Repurpose reserved0 as a GS handle to request GS emulation.
            bytes[cursor + 20..cursor + 24].copy_from_slice(&vs.to_le_bytes());
            patched = true;
            break;
        }

        cursor += size;
    }

    assert!(patched, "failed to find BindShaders command to patch");
}

#[test]
fn aerogpu_cmd_gs_emulation_requires_indirect_execution() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::aerogpu_cmd_gs_emulation_requires_indirect_execution"
        );
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(test_name, &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        // The downlevel "indirect unsupported" error path is only meaningful when compute is
        // available (otherwise the executor will fail earlier with a compute/COMPUTE_SHADERS
        // requirement). Skip cleanly on backends like WebGL2 where compute is unavailable.
        if !exec.caps().supports_compute {
            common::skip_or_panic(
                test_name,
                "backend does not support compute shaders; cannot exercise INDIRECT_EXECUTION-only error path",
            );
            return;
        }

        if exec.supports_indirect() {
            common::skip_or_panic(
                test_name,
                "backend supports INDIRECT_EXECUTION; this test only exercises the downlevel error path",
            );
            return;
        }

        let mut stream = CMD_TRIANGLE_SM4.to_vec();
        patch_first_bind_shaders_gs(&mut stream);

        let mut guest_mem = VecGuestMemory::new(0);
        let err = exec
            .execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect_err("GS emulation should error without indirect execution support");

        let msg = err.to_string();
        assert!(
            msg.contains("INDIRECT_EXECUTION"),
            "error message should mention missing INDIRECT_EXECUTION capability, got: {msg}"
        );
    });
}
