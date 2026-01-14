#![no_main]

use aero_gpu_trace::TraceReader;
use libfuzzer_sys::fuzz_target;
use std::io::Cursor;

/// Cap the raw fuzz input size to keep allocations (both in the harness and the parser) bounded.
const MAX_INPUT_SIZE_BYTES: usize = 1024 * 1024; // 1 MiB

/// Limit the record stream region we ask the parser to scan so pathological inputs can't cause
/// extremely large record counts.
const MAX_RECORD_STREAM_BYTES: usize = 64 * 1024; // 64 KiB

/// Limit the number of TOC frames we attempt to read from any given input.
const MAX_FRAMES: usize = 32;

fn clamp_record_range(file_len: u64, start: u64, end: u64) -> Option<(u64, u64)> {
    let start = start.min(file_len);
    let mut end = end.min(file_len);
    if end <= start {
        return None;
    }
    end = end.min(start.saturating_add(MAX_RECORD_STREAM_BYTES as u64));
    Some((start, end))
}

fn try_parse_trace(bytes: &[u8]) {
    let file_len = bytes.len() as u64;
    let mut reader = match TraceReader::open(Cursor::new(bytes)) {
        Ok(r) => r,
        Err(_) => return,
    };

    let records_start =
        (aero_gpu_trace::TRACE_HEADER_SIZE as u64).saturating_add(reader.meta_json.len() as u64);
    let records_end = reader.footer.toc_offset;
    let record_stream_range = clamp_record_range(file_len, records_start, records_end);

    // Exercise TOC-driven record parsing.
    let entries: Vec<_> = reader
        .frame_entries()
        .iter()
        .take(MAX_FRAMES)
        .copied()
        .collect();
    let mut parsed_record_stream_range = false;
    for entry in entries {
        let Some((start, end)) = clamp_record_range(file_len, entry.start_offset, entry.end_offset)
        else {
            continue;
        };
        if record_stream_range == Some((start, end)) {
            parsed_record_stream_range = true;
        }
        let _ = reader.read_records_in_range(start, end);

        // If the TOC recorded a present offset, try parsing starting from there too. This gives the
        // fuzzer a second entry point into the record stream that is often on a record boundary.
        if entry.present_offset != 0 {
            if let Some((start, end)) =
                clamp_record_range(file_len, entry.present_offset, entry.end_offset)
            {
                let _ = reader.read_records_in_range(start, end);
            }
        }
    }

    // Also try parsing a small prefix of the record stream region (even if the TOC is empty).
    //
    // Avoid duplicating work when a TOC entry already covers this clamped range.
    if !parsed_record_stream_range {
        if let Some((start, end)) = record_stream_range {
            let _ = reader.read_records_in_range(start, end);
        }
    }
}

/// Construct a minimally valid trace container that embeds the fuzzer-provided bytes as the record
/// stream region.
///
/// This makes it much easier for libFuzzer to reach record parsing paths without having to
/// "discover" all the container magic/offset fields.
fn build_synthetic_container(record_stream: &[u8]) -> Vec<u8> {
    const TRACE_MAGIC: [u8; 8] = *b"AEROGPUT";
    const TOC_MAGIC: [u8; 8] = *b"AEROTOC\0";
    const FOOTER_MAGIC: [u8; 8] = *b"AEROGPUF";

    const TOC_VERSION: u32 = 1;
    const TOC_HEADER_SIZE: usize = 16;
    const TOC_ENTRY_SIZE: usize = 32;
    const TOC_LEN_BYTES: u64 = (TOC_HEADER_SIZE + TOC_ENTRY_SIZE) as u64;

    let header_size = aero_gpu_trace::TRACE_HEADER_SIZE as usize;
    let footer_size = aero_gpu_trace::TRACE_FOOTER_SIZE as usize;

    // Keep metadata empty; the fuzzer controls only the record stream bytes.
    let meta_len: u32 = 0;
    let record_stream_start = header_size;
    let record_stream_end = record_stream_start.saturating_add(record_stream.len());
    let toc_offset = record_stream_end as u64;

    let capacity = header_size
        .saturating_add(record_stream.len())
        .saturating_add(TOC_LEN_BYTES as usize)
        .saturating_add(footer_size);
    let mut out = Vec::with_capacity(capacity);

    // TraceHeader (32 bytes)
    out.extend_from_slice(&TRACE_MAGIC);
    out.extend_from_slice(&aero_gpu_trace::TRACE_HEADER_SIZE.to_le_bytes());
    out.extend_from_slice(&aero_gpu_trace::CONTAINER_VERSION.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // command_abi_version
    out.extend_from_slice(&0u32.to_le_bytes()); // flags
    out.extend_from_slice(&meta_len.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // reserved

    // record stream bytes
    out.extend_from_slice(record_stream);

    // TraceToc (48 bytes for 1 entry)
    out.extend_from_slice(&TOC_MAGIC);
    out.extend_from_slice(&TOC_VERSION.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes()); // frame_count
                                                // FrameTocEntry
    out.extend_from_slice(&0u32.to_le_bytes()); // frame_index
    out.extend_from_slice(&0u32.to_le_bytes()); // flags
    out.extend_from_slice(&(record_stream_start as u64).to_le_bytes()); // start_offset
    out.extend_from_slice(&0u64.to_le_bytes()); // present_offset (0 = missing)
    out.extend_from_slice(&(record_stream_end as u64).to_le_bytes()); // end_offset

    // TraceFooter (32 bytes)
    out.extend_from_slice(&FOOTER_MAGIC);
    out.extend_from_slice(&aero_gpu_trace::TRACE_FOOTER_SIZE.to_le_bytes());
    out.extend_from_slice(&aero_gpu_trace::CONTAINER_VERSION.to_le_bytes());
    out.extend_from_slice(&toc_offset.to_le_bytes());
    out.extend_from_slice(&TOC_LEN_BYTES.to_le_bytes());

    out
}

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_SIZE_BYTES {
        return;
    }

    // 1) Treat the input as an on-disk trace file.
    try_parse_trace(data);

    // 2) Also embed the bytes into a minimally valid container so record parsing is exercised
    //    even when the raw input doesn't contain the right magic values.
    let record_len = data.len().min(MAX_RECORD_STREAM_BYTES);
    let trace = build_synthetic_container(&data[..record_len]);
    try_parse_trace(&trace);
});
