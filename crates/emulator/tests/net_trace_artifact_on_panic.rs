#![cfg(not(target_arch = "wasm32"))]

use std::time::{SystemTime, UNIX_EPOCH};

use emulator::io::net::trace::{
    CaptureArtifactOnPanic, FrameDirection, NetTraceConfig, NetTracer,
};

#[test]
fn writes_capture_artifact_when_panicking() {
    let tracer = NetTracer::new(NetTraceConfig::default());
    tracer.enable();

    let frame = [0u8; 14];
    tracer.record_ethernet_at(1, FrameDirection::GuestTx, &frame);

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time went backwards")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("aero-net-trace-{unique}.pcapng"));
    let _ = std::fs::remove_file(&path);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = CaptureArtifactOnPanic::new(&tracer, &path);
        panic!("intentional panic to validate capture artifact emission");
    }));
    assert!(result.is_err());

    let bytes = std::fs::read(&path).expect("expected capture artifact to exist");
    assert!(!bytes.is_empty(), "capture artifact is empty");

    let _ = std::fs::remove_file(&path);
}

