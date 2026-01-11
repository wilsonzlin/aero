use aero_gpu_trace::{TraceMeta, TraceReader, TraceRecord, TraceWriter};
use aero_protocol::aerogpu::aerogpu_cmd::{AerogpuCmdOpcode, AEROGPU_CLEAR_COLOR, AEROGPU_CMD_STREAM_MAGIC};
use aero_protocol::aerogpu::aerogpu_pci::AEROGPU_ABI_VERSION_U32;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

fn fixture_path() -> PathBuf {
    // `CARGO_MANIFEST_DIR` = `.../crates/aero-gpu-trace`
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/aerogpu_a3a0_clear_red.aerogputrace")
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
    // Command stream layout: `aerogpu_cmd_stream_header` (24 bytes) followed by packets.
    let mut packets = Vec::<u8>::new();

    // CLEAR: opaque red.
    // struct aerogpu_cmd_clear (36 bytes)
    packets.extend_from_slice(&(AerogpuCmdOpcode::Clear as u32).to_le_bytes());
    packets.extend_from_slice(&(36u32).to_le_bytes());
    packets.extend_from_slice(&AEROGPU_CLEAR_COLOR.to_le_bytes());
    packets.extend_from_slice(&1.0f32.to_bits().to_le_bytes()); // R
    packets.extend_from_slice(&0.0f32.to_bits().to_le_bytes()); // G
    packets.extend_from_slice(&0.0f32.to_bits().to_le_bytes()); // B
    packets.extend_from_slice(&1.0f32.to_bits().to_le_bytes()); // A
    packets.extend_from_slice(&1.0f32.to_bits().to_le_bytes()); // depth (unused)
    packets.extend_from_slice(&0u32.to_le_bytes()); // stencil (unused)

    // PRESENT: end-of-frame flush.
    // struct aerogpu_cmd_present (16 bytes)
    packets.extend_from_slice(&(AerogpuCmdOpcode::Present as u32).to_le_bytes());
    packets.extend_from_slice(&(16u32).to_le_bytes());
    packets.extend_from_slice(&0u32.to_le_bytes()); // scanout_id
    packets.extend_from_slice(&0u32.to_le_bytes()); // flags

    let size_bytes = (24usize + packets.len()) as u32;

    let mut stream = Vec::<u8>::new();
    stream.extend_from_slice(&AEROGPU_CMD_STREAM_MAGIC.to_le_bytes());
    stream.extend_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
    stream.extend_from_slice(&size_bytes.to_le_bytes());
    stream.extend_from_slice(&0u32.to_le_bytes()); // flags
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved0
    stream.extend_from_slice(&0u32.to_le_bytes()); // reserved1
    stream.extend_from_slice(&packets);

    debug_assert_eq!(stream.len() as u32, size_bytes);
    debug_assert_eq!(stream.len() % 4, 0, "cmd stream must be 4-byte aligned");

    stream
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

