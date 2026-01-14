use aero_gpu_trace::{AerogpuSubmissionCapture, TraceMeta, TraceReader, TraceWriter};
use aero_protocol::aerogpu::aerogpu_cmd::{
    AEROGPU_CLEAR_COLOR, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
};
use aero_protocol::aerogpu::aerogpu_pci::{AerogpuFormat, AEROGPU_ABI_VERSION_U32};
use aero_protocol::aerogpu::aerogpu_ring::AEROGPU_SUBMIT_FLAG_PRESENT;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

fn fixture_path() -> PathBuf {
    // `CARGO_MANIFEST_DIR` = `.../crates/aero-gpu-trace`
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/aerogpu_cmd_clear_b8g8r8a8.aerogputrace")
}

fn make_cmd_stream() -> Vec<u8> {
    const RT: u32 = 1;

    let mut w = AerogpuCmdWriter::new();

    // Use a BGRA format with an *explicit* alpha channel. This exercises:
    // - BGRA render-target mapping + RGBA readback swizzle, and
    // - correct alpha preservation (unlike X8 formats, alpha must not be forced opaque).
    w.create_texture2d(
        RT,
        AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
        AerogpuFormat::B8G8R8A8Unorm as u32,
        64,
        64,
        1,
        1,
        0,
        0,
        0,
    );

    w.set_render_targets(&[RT], 0);
    w.set_viewport(0.0, 0.0, 64.0, 64.0, 0.0, 1.0);

    // Clear red with alpha=0. For A8 formats, alpha should be preserved (not forced opaque).
    w.clear(AEROGPU_CLEAR_COLOR, [1.0, 0.0, 0.0, 0.0], 1.0, 0);
    w.present(0, 0);

    w.finish()
}

fn generate_trace() -> Vec<u8> {
    let cmd_stream = make_cmd_stream();

    let meta = TraceMeta::new("0.0.0-dev", AEROGPU_ABI_VERSION_U32);
    let mut writer = TraceWriter::new_v2(Vec::<u8>::new(), &meta).expect("TraceWriter::new_v2");

    writer.begin_frame(0).unwrap();
    writer
        .write_aerogpu_submission(AerogpuSubmissionCapture {
            submit_flags: AEROGPU_SUBMIT_FLAG_PRESENT,
            context_id: 0,
            engine_id: 0,
            signal_fence: 1,
            cmd_stream_bytes: &cmd_stream,
            alloc_table_bytes: None,
            memory_ranges: &[],
        })
        .unwrap();
    writer.present(0).unwrap();

    let bytes = writer.finish().unwrap();

    // Sanity-check: it must parse, and contain exactly one frame.
    let reader = TraceReader::open(Cursor::new(bytes.clone())).expect("TraceReader::open");
    assert_eq!(reader.header.command_abi_version, AEROGPU_ABI_VERSION_U32);
    assert_eq!(reader.frame_entries().len(), 1);

    bytes
}

#[test]
fn aerogpu_cmd_clear_b8g8r8a8_trace_fixture_is_stable() {
    let bytes = generate_trace();
    let path = fixture_path();

    if std::env::var_os("AERO_UPDATE_TRACE_FIXTURES").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &bytes).unwrap();
        return;
    }

    let fixture =
        fs::read(&path).expect("fixture file missing; run with AERO_UPDATE_TRACE_FIXTURES=1");
    assert_eq!(bytes, fixture);
}
