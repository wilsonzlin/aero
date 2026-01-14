#![cfg(all(feature = "aerogpu-native", not(target_arch = "wasm32")))]

use aero_devices_gpu::{AeroGpuBackendSubmission, AeroGpuCommandBackend, NativeAeroGpuBackend};
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;
use memory::Bus;

#[test]
fn native_backend_smoke_submits_and_completes_fence() {
    let mut backend = match NativeAeroGpuBackend::new_headless() {
        Ok(backend) => backend,
        Err(aero_gpu::AerogpuD3d9Error::AdapterNotFound) => {
            eprintln!("skipping: wgpu adapter not found");
            return;
        }
        Err(err) => panic!("failed to create NativeAeroGpuBackend: {err:?}"),
    };

    let mut mem = Bus::new(0x1000);

    // Trivial but valid command stream: header + FLUSH packet.
    let mut writer = AerogpuCmdWriter::new();
    writer.flush();
    let cmd_stream = writer.finish();

    backend
        .submit(
            &mut mem,
            AeroGpuBackendSubmission {
                flags: 0,
                context_id: 0,
                engine_id: 0,
                signal_fence: 123,
                cmd_stream,
                alloc_table: None,
            },
        )
        .expect("submit failed");

    let completions = backend.poll_completions();
    assert_eq!(completions.len(), 1);
    assert_eq!(completions[0].fence, 123);
    assert_eq!(
        completions[0].error, None,
        "expected command stream to execute without error"
    );
}

