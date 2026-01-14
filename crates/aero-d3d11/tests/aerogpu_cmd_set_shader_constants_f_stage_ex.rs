mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::runtime::bindings::ShaderStage;
use aero_gpu::guest_memory::VecGuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuCmdHdr as ProtocolCmdHdr, AerogpuCmdOpcode,
    AerogpuCmdStreamHeader as ProtocolCmdStreamHeader, AerogpuShaderStageEx,
    AEROGPU_STAGE_EX_MIN_ABI_MINOR,
};
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;

const CMD_TRIANGLE_SM4: &[u8] = include_bytes!("fixtures/cmd_triangle_sm4.bin");

const CMD_STREAM_SIZE_BYTES_OFFSET: usize =
    core::mem::offset_of!(ProtocolCmdStreamHeader, size_bytes);
const CMD_STREAM_ABI_VERSION_OFFSET: usize =
    core::mem::offset_of!(ProtocolCmdStreamHeader, abi_version);
const CMD_HDR_SIZE_BYTES_OFFSET: usize = core::mem::offset_of!(ProtocolCmdHdr, size_bytes);

fn bump_stream_abi_minor(stream: &mut [u8], min_minor: u16) {
    let abi = u32::from_le_bytes(
        stream[CMD_STREAM_ABI_VERSION_OFFSET..CMD_STREAM_ABI_VERSION_OFFSET + 4]
            .try_into()
            .expect("abi_version slice"),
    );
    let major = abi >> 16;
    let minor = (abi & 0xFFFF) as u16;
    if minor >= min_minor {
        return;
    }
    let bumped = (major << 16) | (min_minor as u32);
    stream[CMD_STREAM_ABI_VERSION_OFFSET..CMD_STREAM_ABI_VERSION_OFFSET + 4]
        .copy_from_slice(&bumped.to_le_bytes());
}

fn insert_before_first_present(stream: &mut Vec<u8>, insert: &[u8]) {
    let mut cursor = ProtocolCmdStreamHeader::SIZE_BYTES;
    while cursor + ProtocolCmdHdr::SIZE_BYTES <= stream.len() {
        let opcode = u32::from_le_bytes(stream[cursor..cursor + 4].try_into().unwrap());
        let size = u32::from_le_bytes(
            stream[cursor + CMD_HDR_SIZE_BYTES_OFFSET..cursor + CMD_HDR_SIZE_BYTES_OFFSET + 4]
                .try_into()
                .unwrap(),
        ) as usize;
        if size == 0 || cursor + size > stream.len() {
            break;
        }

        if opcode == AerogpuCmdOpcode::Present as u32
            || opcode == AerogpuCmdOpcode::PresentEx as u32
        {
            stream.splice(cursor..cursor, insert.iter().copied());
            let size_bytes = u32::try_from(stream.len()).expect("patched stream too large");
            stream[CMD_STREAM_SIZE_BYTES_OFFSET..CMD_STREAM_SIZE_BYTES_OFFSET + 4]
                .copy_from_slice(&size_bytes.to_le_bytes());
            return;
        }

        cursor += size;
    }

    panic!("failed to find PRESENT command in fixture");
}

#[test]
fn aerogpu_cmd_set_shader_constants_f_stage_ex_routes_to_hs_ds_buffers() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let mut writer = AerogpuCmdWriter::new();
        writer.set_shader_constants_f_ex(AerogpuShaderStageEx::Hull, 1, &[1.0, 2.0, 3.0, 4.0]);
        writer.set_shader_constants_f_ex(AerogpuShaderStageEx::Domain, 2, &[5.0, 6.0, 7.0, 8.0]);
        let stream = writer.finish();

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");

        let hs = exec
            .read_legacy_constants_f32(ShaderStage::Hull, 1, 1)
            .await
            .expect("read HS legacy constants");
        assert_eq!(hs, vec![1.0, 2.0, 3.0, 4.0]);

        let ds = exec
            .read_legacy_constants_f32(ShaderStage::Domain, 2, 1)
            .await
            .expect("read DS legacy constants");
        assert_eq!(ds, vec![5.0, 6.0, 7.0, 8.0]);

        // Confirm stage_ex writes did not clobber the legacy VS/PS/CS buffers.
        for stage in [
            ShaderStage::Vertex,
            ShaderStage::Pixel,
            ShaderStage::Compute,
        ] {
            let v1 = exec
                .read_legacy_constants_f32(stage, 1, 1)
                .await
                .expect("read legacy constants");
            assert_eq!(v1, vec![0.0; 4], "unexpected data in {stage} buffer");

            let v2 = exec
                .read_legacy_constants_f32(stage, 2, 1)
                .await
                .expect("read legacy constants");
            assert_eq!(v2, vec![0.0; 4], "unexpected data in {stage} buffer");
        }
    });
}

#[test]
fn aerogpu_cmd_set_shader_constants_f_stage_ex_fast_path_does_not_touch_compute() {
    pollster::block_on(async {
        let mut exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        // Patch the triangle fixture so the SET_SHADER_CONSTANTS_F command executes *inside* the
        // render-pass fast path (between DRAW and PRESENT).
        let mut stream = CMD_TRIANGLE_SM4.to_vec();

        let mut insert_writer = AerogpuCmdWriter::new();
        insert_writer.set_shader_constants_f_ex(
            AerogpuShaderStageEx::Hull,
            8,
            &[9.0, 10.0, 11.0, 12.0],
        );
        insert_writer.set_shader_constants_f_ex(
            AerogpuShaderStageEx::Domain,
            9,
            &[13.0, 14.0, 15.0, 16.0],
        );
        let insert_stream = insert_writer.finish();
        let insert_cmds = &insert_stream[ProtocolCmdStreamHeader::SIZE_BYTES..];
        insert_before_first_present(&mut stream, insert_cmds);
        // The fixture command stream is ABI 1.0 (minor 0). `stage_ex` decoding is gated on ABI 1.3+
        // to avoid misinterpreting legacy reserved fields, so bump the minor version when we splice
        // in stage_ex commands.
        bump_stream_abi_minor(&mut stream, AEROGPU_STAGE_EX_MIN_ABI_MINOR);

        let mut guest_mem = VecGuestMemory::new(0);
        exec.execute_cmd_stream(&stream, None, &mut guest_mem)
            .expect("execute_cmd_stream should succeed");

        let hs = exec
            .read_legacy_constants_f32(ShaderStage::Hull, 8, 1)
            .await
            .expect("read HS legacy constants");
        assert_eq!(hs, vec![9.0, 10.0, 11.0, 12.0]);

        let ds = exec
            .read_legacy_constants_f32(ShaderStage::Domain, 9, 1)
            .await
            .expect("read DS legacy constants");
        assert_eq!(ds, vec![13.0, 14.0, 15.0, 16.0]);

        // Regression check: stage_ex updates must not be routed to the compute legacy buffer.
        let cs_hs = exec
            .read_legacy_constants_f32(ShaderStage::Compute, 8, 1)
            .await
            .expect("read CS legacy constants");
        assert_eq!(cs_hs, vec![0.0; 4]);
        let cs_ds = exec
            .read_legacy_constants_f32(ShaderStage::Compute, 9, 1)
            .await
            .expect("read CS legacy constants");
        assert_eq!(cs_ds, vec![0.0; 4]);
    });
}
