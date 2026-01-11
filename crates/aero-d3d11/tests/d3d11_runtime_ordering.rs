mod common;

use aero_d3d11::runtime::execute::D3D11Runtime;
use aero_gpu::protocol_d3d11::{BufferUsage, CmdWriter};

#[test]
fn d3d11_runtime_preserves_update_buffer_copy_ordering() {
    pollster::block_on(async {
        let test_name = concat!(
            module_path!(),
            "::d3d11_runtime_preserves_update_buffer_copy_ordering"
        );

        let mut rt = match D3D11Runtime::new_for_tests().await {
            Ok(rt) => rt,
            Err(err) => {
                common::skip_or_panic(test_name, &format!("wgpu unavailable ({err:#})"));
                return;
            }
        };

        const SRC: u32 = 1;
        const DST1: u32 = 2;
        const DST2: u32 = 3;

        let size = 16u64;
        let pattern_a = vec![0x11u8; size as usize];
        let pattern_b = vec![0x22u8; size as usize];

        let mut w = CmdWriter::new();
        w.create_buffer(SRC, size, BufferUsage::COPY_SRC | BufferUsage::COPY_DST);
        w.create_buffer(DST1, size, BufferUsage::MAP_READ | BufferUsage::COPY_DST);
        w.create_buffer(DST2, size, BufferUsage::MAP_READ | BufferUsage::COPY_DST);
        w.update_buffer(SRC, 0, &pattern_a);
        w.copy_buffer_to_buffer(SRC, 0, DST1, 0, size);
        w.update_buffer(SRC, 0, &pattern_b);
        w.copy_buffer_to_buffer(SRC, 0, DST2, 0, size);

        rt.execute(&w.finish()).unwrap();
        rt.poll_wait();

        let got1 = rt.read_buffer(DST1, 0, size).await.unwrap();
        let got2 = rt.read_buffer(DST2, 0, size).await.unwrap();

        assert_eq!(got1, pattern_a, "dst1 should match first update");
        assert_eq!(got2, pattern_b, "dst2 should match second update");
    });
}
