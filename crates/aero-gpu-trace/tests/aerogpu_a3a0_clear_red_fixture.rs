use aero_gpu_trace::{TraceMeta, TraceReader, TraceRecord, TraceWriter};
use aero_protocol::aerogpu::aerogpu_cmd::AEROGPU_CLEAR_COLOR;
use aero_protocol::aerogpu::aerogpu_pci::AEROGPU_ABI_VERSION_U32;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

fn fixture_path() -> PathBuf {
    // `CARGO_MANIFEST_DIR` = `.../crates/aero-gpu-trace`
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/aerogpu_a3a0_clear_red.aerogputrace")
}

fn generate_aerogpu_a3a0_clear_red_trace() -> Vec<u8> {
    let meta = TraceMeta::new("0.0.0-dev", AEROGPU_ABI_VERSION_U32);
    let mut writer = TraceWriter::new(Vec::<u8>::new(), &meta).expect("TraceWriter::new");

    let cmd_stream = make_a3a0_clear_red_cmd_stream();

    writer.begin_frame(0).unwrap();
    writer.write_packet(&cmd_stream).unwrap();
    writer.present(0).unwrap();

    writer.finish().unwrap()
}

fn make_a3a0_clear_red_cmd_stream() -> Vec<u8> {
    let mut w = AerogpuCmdWriter::new();
    w.clear(AEROGPU_CLEAR_COLOR, [1.0, 0.0, 0.0, 1.0], 1.0, 0);
    w.present(0, 0);
    w.finish()
}

#[test]
fn aerogpu_a3a0_clear_red_trace_fixture_is_stable() {
    let bytes = generate_aerogpu_a3a0_clear_red_trace();

    // Sanity-check: it must parse, and contain exactly one frame.
    let mut reader = TraceReader::open(Cursor::new(bytes.clone())).expect("TraceReader::open");
    assert_eq!(reader.header.command_abi_version, AEROGPU_ABI_VERSION_U32);
    assert_eq!(reader.frame_entries().len(), 1);

    // Ensure the trace is "exactly what we think it is" (one frame, one packet, present).
    let cmd_stream = make_a3a0_clear_red_cmd_stream();
    let entry = &reader.frame_entries()[0];
    let records = reader
        .read_records_in_range(entry.start_offset, entry.end_offset)
        .expect("TraceReader::read_records_in_range");
    assert_eq!(
        records,
        vec![
            TraceRecord::BeginFrame { frame_index: 0 },
            TraceRecord::Packet {
                bytes: cmd_stream.clone()
            },
            TraceRecord::Present { frame_index: 0 },
        ]
    );

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
