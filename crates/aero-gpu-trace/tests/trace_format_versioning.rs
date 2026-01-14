use aero_gpu_trace::{
    AerogpuSubmissionCapture, BlobKind, TraceMeta, TraceReadError, TraceReader, TraceRecord,
    TraceWriteError, TraceWriter, CONTAINER_VERSION, TRACE_FOOTER_SIZE, TRACE_HEADER_SIZE,
};
use std::io::Cursor;

fn minimal_trace_bytes(command_abi_version: u32) -> Vec<u8> {
    let meta = TraceMeta::new("test", command_abi_version);
    let mut writer = TraceWriter::new(Vec::<u8>::new(), &meta).expect("TraceWriter::new");
    writer.begin_frame(0).unwrap();
    writer.present(0).unwrap();
    writer.finish().unwrap()
}

fn trace_with_blob(command_abi_version: u32) -> Vec<u8> {
    let meta = TraceMeta::new("test", command_abi_version);
    let mut writer = TraceWriter::new(Vec::<u8>::new(), &meta).expect("TraceWriter::new");
    writer.begin_frame(0).unwrap();
    let _id = writer
        .write_blob(BlobKind::BufferData, b"blob data")
        .unwrap();
    writer.present(0).unwrap();
    writer.finish().unwrap()
}

fn trace_with_aerogpu_submission_v2(command_abi_version: u32) -> Vec<u8> {
    let meta = TraceMeta::new("test", command_abi_version);
    let mut writer = TraceWriter::new_v2(Vec::<u8>::new(), &meta).expect("TraceWriter::new_v2");
    writer.begin_frame(0).unwrap();
    writer
        .write_aerogpu_submission(AerogpuSubmissionCapture {
            submit_flags: 0,
            context_id: 0,
            engine_id: 0,
            signal_fence: 0,
            cmd_stream_bytes: b"dummy cmd stream bytes",
            alloc_table_bytes: None,
            memory_ranges: &[],
        })
        .unwrap();
    writer.present(0).unwrap();
    writer.finish().unwrap()
}

fn read_u32_le(bytes: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap())
}

fn read_u64_le(bytes: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(bytes[off..off + 8].try_into().unwrap())
}

#[test]
fn read_records_in_range_rejects_invalid_ranges() {
    let bytes = minimal_trace_bytes(0);
    let file_len = bytes.len() as u64;

    // start > end
    {
        let mut reader = TraceReader::open(Cursor::new(bytes.clone())).expect("TraceReader::open");
        let err = reader.read_records_in_range(10, 9).unwrap_err();
        assert!(matches!(err, TraceReadError::RecordOutOfBounds));
    }

    // end beyond file length
    {
        let mut reader = TraceReader::open(Cursor::new(bytes.clone())).expect("TraceReader::open");
        let err = reader.read_records_in_range(0, file_len + 1).unwrap_err();
        assert!(matches!(err, TraceReadError::RecordOutOfBounds));
    }

    // start beyond file length
    {
        let mut reader = TraceReader::open(Cursor::new(bytes)).expect("TraceReader::open");
        let err = reader
            .read_records_in_range(file_len + 1, file_len + 1)
            .unwrap_err();
        assert!(matches!(err, TraceReadError::RecordOutOfBounds));
    }
}

#[test]
fn writer_rejects_unknown_container_version() {
    let meta = TraceMeta::new("test", 0);
    let bad_version = CONTAINER_VERSION + 1;
    let err = match TraceWriter::new_with_container_version(Vec::<u8>::new(), &meta, bad_version) {
        Ok(_) => panic!("expected TraceWriter creation to fail"),
        Err(err) => err,
    };
    assert!(matches!(
        err,
        TraceWriteError::UnsupportedContainerVersion(v) if v == bad_version
    ));
}

#[test]
fn writer_rejects_aerogpu_submission_in_container_v1() {
    let meta = TraceMeta::new("test", 0);
    let mut writer = TraceWriter::new(Vec::<u8>::new(), &meta).expect("TraceWriter::new");
    writer.begin_frame(0).unwrap();
    let err = writer
        .write_aerogpu_submission(AerogpuSubmissionCapture {
            submit_flags: 0,
            context_id: 0,
            engine_id: 0,
            signal_fence: 0,
            cmd_stream_bytes: b"dummy cmd stream bytes",
            alloc_table_bytes: None,
            memory_ranges: &[],
        })
        .unwrap_err();
    assert!(matches!(
        err,
        TraceWriteError::UnsupportedContainerVersion(1)
    ));
}

#[test]
fn reject_trace_with_toc_offset_out_of_bounds() {
    let mut bytes = minimal_trace_bytes(0);

    let footer_size = TRACE_FOOTER_SIZE as usize;
    let footer_start = bytes.len() - footer_size;
    let file_len = bytes.len() as u64;

    // Patch footer.toc_offset to point past EOF.
    bytes[footer_start + 16..footer_start + 24].copy_from_slice(&file_len.to_le_bytes());

    let err = match TraceReader::open(Cursor::new(bytes)) {
        Ok(_) => panic!("expected trace open to fail"),
        Err(err) => err,
    };
    assert!(matches!(err, TraceReadError::TocOutOfBounds));
}

#[test]
fn reject_trace_with_toc_len_smaller_than_expected() {
    let mut bytes = minimal_trace_bytes(0);

    let footer_size = TRACE_FOOTER_SIZE as usize;
    let footer_start = bytes.len() - footer_size;

    // Set toc_len to the TOC header size only (16). This is enough to read the header but
    // mismatches the expected length once frame_count is read.
    bytes[footer_start + 24..footer_start + 32].copy_from_slice(&16u64.to_le_bytes());

    let err = match TraceReader::open(Cursor::new(bytes)) {
        Ok(_) => panic!("expected trace open to fail"),
        Err(err) => err,
    };
    assert!(matches!(err, TraceReadError::TocOutOfBounds));
}

#[test]
fn reject_trace_with_toc_entry_out_of_bounds() {
    let mut bytes = minimal_trace_bytes(0);

    let footer_size = TRACE_FOOTER_SIZE as usize;
    let footer_start = bytes.len() - footer_size;
    let toc_offset = read_u64_le(&bytes, footer_start + 16) as usize;

    // Corrupt the first TOC entry's start_offset to point before the record stream.
    //
    // TOC entry layout:
    // u32 frame_index;
    // u32 flags;
    // u64 start_offset;
    // u64 present_offset;
    // u64 end_offset;
    let entry_start = toc_offset + 16; // TOC_HEADER_SIZE
    let start_offset_field = entry_start + 8;
    bytes[start_offset_field..start_offset_field + 8].copy_from_slice(&0u64.to_le_bytes());

    let err = match TraceReader::open(Cursor::new(bytes)) {
        Ok(_) => panic!("expected trace open to fail"),
        Err(err) => err,
    };
    assert!(matches!(err, TraceReadError::TocOutOfBounds));
}

#[test]
fn reject_trace_with_toc_entry_end_before_start() {
    let mut bytes = minimal_trace_bytes(0);

    let footer_size = TRACE_FOOTER_SIZE as usize;
    let footer_start = bytes.len() - footer_size;
    let toc_offset = read_u64_le(&bytes, footer_start + 16) as usize;

    // Corrupt the first TOC entry's end_offset to be 0, which must be < start_offset.
    let entry_start = toc_offset + 16; // TOC_HEADER_SIZE
    let end_offset_field = entry_start + 24;
    bytes[end_offset_field..end_offset_field + 8].copy_from_slice(&0u64.to_le_bytes());

    let err = match TraceReader::open(Cursor::new(bytes)) {
        Ok(_) => panic!("expected trace open to fail"),
        Err(err) => err,
    };
    assert!(matches!(err, TraceReadError::TocOutOfBounds));
}

#[test]
fn reject_trace_with_toc_entry_present_out_of_bounds() {
    let mut bytes = minimal_trace_bytes(0);

    let footer_size = TRACE_FOOTER_SIZE as usize;
    let footer_start = bytes.len() - footer_size;
    let toc_offset = read_u64_le(&bytes, footer_start + 16) as usize;

    // Set present_offset to a non-zero value outside the frame range.
    let entry_start = toc_offset + 16; // TOC_HEADER_SIZE
    let present_offset_field = entry_start + 16;
    bytes[present_offset_field..present_offset_field + 8].copy_from_slice(&u64::MAX.to_le_bytes());

    let err = match TraceReader::open(Cursor::new(bytes)) {
        Ok(_) => panic!("expected trace open to fail"),
        Err(err) => err,
    };
    assert!(matches!(err, TraceReadError::TocOutOfBounds));
}

#[test]
fn reject_trace_with_meta_len_overlapping_record_stream() {
    let mut bytes = minimal_trace_bytes(0);

    // Read the real toc_offset from the footer, then corrupt meta_len so that:
    //   record_stream_start = TRACE_HEADER_SIZE + meta_len
    // becomes *after* toc_offset, which is invalid.
    let footer_size = TRACE_FOOTER_SIZE as usize;
    let footer_start = bytes.len() - footer_size;
    let toc_offset = read_u64_le(&bytes, footer_start + 16);

    let meta_len = toc_offset
        .checked_sub(TRACE_HEADER_SIZE as u64)
        .unwrap()
        .checked_add(1)
        .unwrap();
    bytes[24..28].copy_from_slice(&(meta_len as u32).to_le_bytes());

    let err = match TraceReader::open(Cursor::new(bytes)) {
        Ok(_) => panic!("expected trace open to fail"),
        Err(err) => err,
    };
    assert!(matches!(err, TraceReadError::TocOutOfBounds));
}

#[test]
fn reject_trace_with_meta_len_out_of_bounds() {
    let mut bytes = minimal_trace_bytes(0);

    // TraceHeader layout (little-endian):
    // [0..8]   magic
    // [8..12]  header_size
    // [12..16] container_version
    // [16..20] command_abi_version
    // [20..24] flags
    // [24..28] meta_len
    bytes[24..28].copy_from_slice(&u32::MAX.to_le_bytes());

    let err = match TraceReader::open(Cursor::new(bytes)) {
        Ok(_) => panic!("expected trace open to fail"),
        Err(err) => err,
    };
    assert!(matches!(err, TraceReadError::RecordOutOfBounds));
}

#[test]
fn reject_trace_with_toc_len_mismatch() {
    let mut bytes = minimal_trace_bytes(0);

    let footer_size = TRACE_FOOTER_SIZE as usize;
    let footer_start = bytes.len() - footer_size;

    // Patch footer.toc_len to something too small to contain even the TOC header.
    bytes[footer_start + 24..footer_start + 32].copy_from_slice(&0u64.to_le_bytes());

    let err = match TraceReader::open(Cursor::new(bytes)) {
        Ok(_) => panic!("expected trace open to fail"),
        Err(err) => err,
    };
    assert!(matches!(err, TraceReadError::TocOutOfBounds));
}

#[test]
fn reject_trace_with_toc_offset_len_overflow() {
    let mut bytes = minimal_trace_bytes(0);

    let footer_size = TRACE_FOOTER_SIZE as usize;
    let footer_start = bytes.len() - footer_size;

    // Set toc_offset and toc_len such that toc_offset + toc_len overflows u64.
    bytes[footer_start + 16..footer_start + 24].copy_from_slice(&u64::MAX.to_le_bytes());
    bytes[footer_start + 24..footer_start + 32].copy_from_slice(&1u64.to_le_bytes());

    let err = match TraceReader::open(Cursor::new(bytes)) {
        Ok(_) => panic!("expected trace open to fail"),
        Err(err) => err,
    };
    assert!(matches!(err, TraceReadError::TocOutOfBounds));
}

#[test]
fn reject_trace_with_unsupported_header_size() {
    let mut bytes = minimal_trace_bytes(0);

    // TraceHeader layout (little-endian):
    // [0..8]   magic
    // [8..12]  header_size
    bytes[8..12].copy_from_slice(&0u32.to_le_bytes());

    let err = match TraceReader::open(Cursor::new(bytes)) {
        Ok(_) => panic!("expected trace open to fail"),
        Err(err) => err,
    };
    assert!(matches!(err, TraceReadError::UnsupportedHeaderSize(0)));
}

#[test]
fn reject_trace_with_wrong_magic() {
    let mut bytes = minimal_trace_bytes(0);

    // Corrupt the first byte of the 8-byte trace header magic.
    bytes[0] ^= 0xFF;

    let err = match TraceReader::open(Cursor::new(bytes)) {
        Ok(_) => panic!("expected trace open to fail"),
        Err(err) => err,
    };
    assert!(matches!(err, TraceReadError::InvalidMagic));
}

#[test]
fn reject_trace_with_unknown_newer_container_version() {
    let mut bytes = minimal_trace_bytes(0);

    // TraceHeader layout (little-endian):
    // [0..8]   magic
    // [8..12]  header_size
    // [12..16] container_version
    let bad_version = CONTAINER_VERSION + 1;
    bytes[12..16].copy_from_slice(&bad_version.to_le_bytes());

    let err = match TraceReader::open(Cursor::new(bytes)) {
        Ok(_) => panic!("expected trace open to fail"),
        Err(err) => err,
    };
    assert!(matches!(
        err,
        TraceReadError::UnsupportedContainerVersion(v) if v == bad_version
    ));
}

#[test]
fn reject_trace_with_too_old_container_version() {
    let mut bytes = minimal_trace_bytes(0);

    // Set header container_version = 0 (unsupported).
    bytes[12..16].copy_from_slice(&0u32.to_le_bytes());

    let err = match TraceReader::open(Cursor::new(bytes)) {
        Ok(_) => panic!("expected trace open to fail"),
        Err(err) => err,
    };
    assert!(matches!(
        err,
        TraceReadError::UnsupportedContainerVersion(0)
    ));
}

#[test]
fn reject_trace_with_too_old_footer_container_version() {
    let mut bytes = minimal_trace_bytes(0);

    let footer_size = TRACE_FOOTER_SIZE as usize;
    let footer_start = bytes.len() - footer_size;

    // Set footer container_version = 0 (unsupported). The reader should reject before comparing
    // header/footer versions.
    bytes[footer_start + 12..footer_start + 16].copy_from_slice(&0u32.to_le_bytes());

    let err = match TraceReader::open(Cursor::new(bytes)) {
        Ok(_) => panic!("expected trace open to fail"),
        Err(err) => err,
    };
    assert!(matches!(
        err,
        TraceReadError::UnsupportedContainerVersion(0)
    ));
}

#[test]
fn reject_trace_with_unknown_newer_footer_container_version() {
    let mut bytes = minimal_trace_bytes(0);

    let footer_size = TRACE_FOOTER_SIZE as usize;
    let footer_start = bytes.len() - footer_size;

    // Set footer container_version to an unknown newer version.
    let bad_version = CONTAINER_VERSION + 1;
    bytes[footer_start + 12..footer_start + 16].copy_from_slice(&bad_version.to_le_bytes());

    let err = match TraceReader::open(Cursor::new(bytes)) {
        Ok(_) => panic!("expected trace open to fail"),
        Err(err) => err,
    };
    assert!(matches!(
        err,
        TraceReadError::UnsupportedContainerVersion(v) if v == bad_version
    ));
}

#[test]
fn reject_unknown_record_type_in_supported_container_version() {
    let mut bytes = minimal_trace_bytes(0);

    let meta_len = read_u32_le(&bytes, 24) as usize;
    let record_stream_start = 32 + meta_len;

    // First record is BeginFrame; corrupt its record_type byte.
    bytes[record_stream_start] = 0xFF;

    let mut reader = TraceReader::open(Cursor::new(bytes)).expect("TraceReader::open");
    let entry = reader.frame_entries()[0];
    let err = reader
        .read_records_in_range(entry.start_offset, entry.end_offset)
        .unwrap_err();
    assert!(matches!(err, TraceReadError::UnknownRecordType(0xFF)));
}

#[test]
fn reject_record_payload_out_of_bounds() {
    let mut bytes = minimal_trace_bytes(0);

    let meta_len = read_u32_le(&bytes, 24) as usize;
    let record_stream_start = 32 + meta_len;

    // BeginFrame record header starts at `record_stream_start`. Patch its payload_len to a huge
    // value so the record would extend past the frame end.
    bytes[record_stream_start + 4..record_stream_start + 8]
        .copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());

    let mut reader = TraceReader::open(Cursor::new(bytes)).expect("TraceReader::open");
    let entry = reader.frame_entries()[0];
    let err = reader
        .read_records_in_range(entry.start_offset, entry.end_offset)
        .unwrap_err();
    assert!(matches!(err, TraceReadError::RecordOutOfBounds));
}

#[test]
fn reject_unknown_blob_kind() {
    let mut bytes = trace_with_blob(0);

    let meta_len = read_u32_le(&bytes, 24) as usize;
    let record_stream_start = 32 + meta_len;

    // Record layout: BeginFrame (12 bytes) then Blob.
    let blob_record_start = record_stream_start + 12;
    // Blob payload starts after the 8-byte record header. BlobKind is at payload + 8.
    let blob_kind_off = blob_record_start + 8 + 8;
    bytes[blob_kind_off..blob_kind_off + 4].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());

    let mut reader = TraceReader::open(Cursor::new(bytes)).expect("TraceReader::open");
    let entry = reader.frame_entries()[0];
    let err = reader
        .read_records_in_range(entry.start_offset, entry.end_offset)
        .unwrap_err();
    assert!(matches!(
        err,
        TraceReadError::UnknownBlobKind(v) if v == 0xDEAD_BEEF
    ));
}

#[test]
fn reject_malformed_blob_record() {
    let mut bytes = trace_with_blob(0);

    let footer_size = TRACE_FOOTER_SIZE as usize;
    let footer_start = bytes.len() - footer_size;
    let toc_offset = read_u64_le(&bytes, footer_start + 16) as usize;

    let meta_len = read_u32_le(&bytes, 24) as usize;
    let record_stream_start = 32 + meta_len;

    // BeginFrame record is always 12 bytes (8 header + 4 payload).
    let blob_record_start = record_stream_start + 12;

    // Corrupt the blob record to have payload_len = 0 (< 16-byte blob header).
    bytes[blob_record_start + 4..blob_record_start + 8].copy_from_slice(&0u32.to_le_bytes());

    // Patch the TOC entry to:
    // - clear present_offset (0)
    // - clamp end_offset to the end of the truncated blob record
    let entry_start = toc_offset + 16;
    let present_offset_field = entry_start + 16;
    let end_offset_field = entry_start + 24;
    bytes[present_offset_field..present_offset_field + 8].copy_from_slice(&0u64.to_le_bytes());
    let truncated_end = (blob_record_start + 8) as u64;
    bytes[end_offset_field..end_offset_field + 8].copy_from_slice(&truncated_end.to_le_bytes());

    let mut reader = TraceReader::open(Cursor::new(bytes)).expect("TraceReader::open");
    let entry = reader.frame_entries()[0];
    let err = reader
        .read_records_in_range(entry.start_offset, entry.end_offset)
        .unwrap_err();
    assert!(matches!(err, TraceReadError::MalformedBlob));
}

#[test]
fn reject_trace_with_wrong_footer_magic() {
    let mut bytes = minimal_trace_bytes(0);

    let footer_size = TRACE_FOOTER_SIZE as usize;
    let footer_start = bytes.len() - footer_size;
    bytes[footer_start] ^= 0xFF;

    let err = match TraceReader::open(Cursor::new(bytes)) {
        Ok(_) => panic!("expected trace open to fail"),
        Err(err) => err,
    };
    assert!(matches!(err, TraceReadError::InvalidMagic));
}

#[test]
fn reject_trace_with_unsupported_footer_size() {
    let mut bytes = minimal_trace_bytes(0);

    let footer_size = TRACE_FOOTER_SIZE as usize;
    let footer_start = bytes.len() - footer_size;

    // TraceFooter layout (little-endian):
    // [0..8]   magic
    // [8..12]  footer_size
    bytes[footer_start + 8..footer_start + 12].copy_from_slice(&0u32.to_le_bytes());

    let err = match TraceReader::open(Cursor::new(bytes)) {
        Ok(_) => panic!("expected trace open to fail"),
        Err(err) => err,
    };
    assert!(matches!(err, TraceReadError::UnsupportedFooterSize(0)));
}

#[test]
fn reject_trace_with_wrong_toc_magic() {
    let mut bytes = minimal_trace_bytes(0);

    let footer_size = TRACE_FOOTER_SIZE as usize;
    let footer_start = bytes.len() - footer_size;
    let toc_offset = read_u64_le(&bytes, footer_start + 16) as usize;
    let _toc_len = read_u64_le(&bytes, footer_start + 24) as usize;

    // Corrupt TOC magic at `toc_offset`.
    bytes[toc_offset] ^= 0xFF;

    let err = match TraceReader::open(Cursor::new(bytes)) {
        Ok(_) => panic!("expected trace open to fail"),
        Err(err) => err,
    };
    assert!(matches!(err, TraceReadError::InvalidMagic));
}

#[test]
fn reject_trace_with_unsupported_toc_version() {
    let mut bytes = minimal_trace_bytes(0);

    let footer_size = TRACE_FOOTER_SIZE as usize;
    let footer_start = bytes.len() - footer_size;
    let toc_offset = read_u64_le(&bytes, footer_start + 16) as usize;

    // TOC layout:
    // [0..8]  magic
    // [8..12] toc_version
    bytes[toc_offset + 8..toc_offset + 12].copy_from_slice(&999u32.to_le_bytes());

    let err = match TraceReader::open(Cursor::new(bytes)) {
        Ok(_) => panic!("expected trace open to fail"),
        Err(err) => err,
    };
    assert!(matches!(err, TraceReadError::UnsupportedTocVersion(999)));
}

#[test]
fn reject_aerogpu_submission_record_in_container_v1() {
    // Start from a valid v2 trace containing an AerogpuSubmission record, then patch
    // `container_version` down to v1. The reader should reject the v2-only record type.
    let mut bytes = trace_with_aerogpu_submission_v2(0);

    // Patch header container_version.
    bytes[12..16].copy_from_slice(&1u32.to_le_bytes());

    // Patch footer container_version.
    let footer_size = TRACE_FOOTER_SIZE as usize;
    let footer_start = bytes.len() - footer_size;
    bytes[footer_start + 12..footer_start + 16].copy_from_slice(&1u32.to_le_bytes());

    let mut reader = TraceReader::open(Cursor::new(bytes)).expect("TraceReader::open");
    let entry = reader.frame_entries()[0];
    let err = reader
        .read_records_in_range(entry.start_offset, entry.end_offset)
        .unwrap_err();
    assert!(matches!(err, TraceReadError::UnknownRecordType(0x05)));
}

#[test]
fn accept_aerogpu_submission_record_in_container_v2() {
    let bytes = trace_with_aerogpu_submission_v2(0);

    let mut reader = TraceReader::open(Cursor::new(bytes)).expect("TraceReader::open");
    let entry = reader.frame_entries()[0];
    let records = reader
        .read_records_in_range(entry.start_offset, entry.end_offset)
        .expect("TraceReader::read_records_in_range");

    assert_eq!(records.len(), 4);
    assert_eq!(records[0], TraceRecord::BeginFrame { frame_index: 0 });
    match &records[1] {
        TraceRecord::Blob {
            blob_id,
            kind,
            bytes,
        } => {
            assert_eq!(*blob_id, 1);
            assert_eq!(*kind, BlobKind::AerogpuCmdStream);
            assert_eq!(bytes, b"dummy cmd stream bytes");
        }
        other => panic!("expected blob record, got {other:?}"),
    }
    match &records[2] {
        TraceRecord::AerogpuSubmission {
            record_version,
            submit_flags,
            context_id,
            engine_id,
            signal_fence,
            cmd_stream_blob_id,
            alloc_table_blob_id,
            memory_ranges,
        } => {
            assert_eq!(*record_version, 1);
            assert_eq!(*submit_flags, 0);
            assert_eq!(*context_id, 0);
            assert_eq!(*engine_id, 0);
            assert_eq!(*signal_fence, 0);
            assert_eq!(*cmd_stream_blob_id, 1);
            assert_eq!(*alloc_table_blob_id, 0);
            assert!(memory_ranges.is_empty());
        }
        other => panic!("expected aerogpu submission record, got {other:?}"),
    }
    assert_eq!(records[3], TraceRecord::Present { frame_index: 0 });
}

#[test]
fn reject_trace_with_header_footer_version_mismatch() {
    let mut bytes = minimal_trace_bytes(0);

    // Patch footer `container_version` to a different *supported* version, so we validate the
    // mismatch path (as opposed to "unsupported version" early exits).
    let footer_size = TRACE_FOOTER_SIZE as usize;
    let footer_start = bytes.len() - footer_size;
    let header_version = read_u32_le(&bytes, 12);
    let mismatched_footer_version: u32 = if header_version == 1 { 2 } else { 1 };
    bytes[footer_start + 12..footer_start + 16]
        .copy_from_slice(&mismatched_footer_version.to_le_bytes());

    let err = match TraceReader::open(Cursor::new(bytes)) {
        Ok(_) => panic!("expected trace open to fail"),
        Err(err) => err,
    };
    assert!(matches!(
        err,
        TraceReadError::UnsupportedContainerVersion(v) if v == mismatched_footer_version
    ));
}

#[test]
fn accept_trace_with_older_aerogpu_abi_minor_version() {
    use aero_protocol::aerogpu::aerogpu_pci::{
        abi_major, abi_minor, AEROGPU_ABI_MAJOR, AEROGPU_ABI_VERSION_U32,
    };

    // The AeroGPU protocol defines ABI minor versions as backwards-compatible extensions.
    // A trace recorded against an older minor version should still be readable by the trace
    // container reader (the container stores the value but does not interpret packet bytes).
    let older_minor_version: u32 = AEROGPU_ABI_MAJOR << 16;

    // Assert this really is an "older or equal minor" version of the current ABI major.
    assert_eq!(
        abi_major(older_minor_version),
        abi_major(AEROGPU_ABI_VERSION_U32)
    );
    assert!(abi_minor(older_minor_version) <= abi_minor(AEROGPU_ABI_VERSION_U32));

    let bytes = minimal_trace_bytes(older_minor_version);
    let reader = TraceReader::open(Cursor::new(bytes)).expect("TraceReader::open");
    assert_eq!(reader.header.command_abi_version, older_minor_version);
}
