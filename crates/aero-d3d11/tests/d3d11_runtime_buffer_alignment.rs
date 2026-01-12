mod common;

use aero_d3d11::runtime::execute::D3D11Runtime;
use aero_gpu::protocol_d3d11::{BufferUsage, CmdWriter};

#[test]
fn d3d11_runtime_rejects_unaligned_copy_buffer_to_buffer() {
    pollster::block_on(async {
        let mut rt = match D3D11Runtime::new_for_tests().await {
            Ok(rt) => rt,
            Err(err) => {
                common::skip_or_panic(
                    concat!(
                        module_path!(),
                        "::d3d11_runtime_rejects_unaligned_copy_buffer_to_buffer"
                    ),
                    &format!("wgpu unavailable ({err:#})"),
                );
                return;
            }
        };

        const SRC: u32 = 1;
        const DST: u32 = 2;

        let mut writer = CmdWriter::new();
        writer.create_buffer(SRC, 16, BufferUsage::COPY_SRC | BufferUsage::COPY_DST);
        writer.create_buffer(DST, 16, BufferUsage::COPY_SRC | BufferUsage::COPY_DST);
        writer.copy_buffer_to_buffer(SRC, 0, DST, 0, 2);
        let words = writer.finish();

        let err = rt
            .execute(&words)
            .expect_err("unaligned copy should be rejected");
        assert!(
            err.to_string().contains("CopyBufferToBuffer"),
            "unexpected error: {err:#}"
        );
    });
}

#[test]
fn d3d11_runtime_update_buffer_succeeds_without_copy_dst_usage_flag() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::d3d11_runtime_update_buffer_succeeds_without_copy_dst_usage_flag"
        );
        let mut rt = match D3D11Runtime::new_for_tests().await {
            Ok(rt) => rt,
            Err(err) => {
                common::skip_or_panic(test_name, &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };

        let mut writer = CmdWriter::new();
        writer.create_buffer(1, 4, BufferUsage::UNIFORM); // deliberately omit COPY_DST
        writer.update_buffer(1, 0, &[1u8, 2u8, 3u8, 4u8]);

        rt.execute(&writer.finish())
            .expect("UpdateBuffer should succeed even if COPY_DST is omitted in CreateBuffer usage");
    });
}
