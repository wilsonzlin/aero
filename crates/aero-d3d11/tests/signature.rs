use aero_d3d11::{parse_signature_chunk, parse_signatures, DxbcFile, FourCC};

fn build_signature_chunk_v0_one_entry(semantic_name: &str, register: u32) -> Vec<u8> {
    let mut bytes = Vec::new();

    let param_count = 1u32;
    let param_offset = 8u32;

    bytes.extend_from_slice(&param_count.to_le_bytes());
    bytes.extend_from_slice(&param_offset.to_le_bytes());

    let table_start = bytes.len();
    assert_eq!(table_start, 8);

    let entry_size = 24usize;
    let string_table_offset = (table_start + entry_size) as u32;

    bytes.extend_from_slice(&string_table_offset.to_le_bytes()); // semantic_name_offset
    bytes.extend_from_slice(&0u32.to_le_bytes()); // semantic_index
    bytes.extend_from_slice(&0u32.to_le_bytes()); // system_value_type
    bytes.extend_from_slice(&3u32.to_le_bytes()); // component_type (float32)
    bytes.extend_from_slice(&register.to_le_bytes());
    bytes.extend_from_slice(&u32::from_le_bytes([0xF, 0xF, 0, 0]).to_le_bytes()); // mask/rw/stream/min_prec

    bytes.extend_from_slice(semantic_name.as_bytes());
    bytes.push(0);

    bytes
}

fn build_signature_chunk_v1_one_entry(semantic_name: &str, register: u32, stream: u32) -> Vec<u8> {
    let mut bytes = Vec::new();

    let param_count = 1u32;
    let param_offset = 8u32;

    bytes.extend_from_slice(&param_count.to_le_bytes());
    bytes.extend_from_slice(&param_offset.to_le_bytes());

    let table_start = bytes.len();
    assert_eq!(table_start, 8);

    let entry_size = 32usize;
    let string_table_offset = (table_start + entry_size) as u32;

    bytes.extend_from_slice(&string_table_offset.to_le_bytes()); // semantic_name_offset
    bytes.extend_from_slice(&0u32.to_le_bytes()); // semantic_index
    bytes.extend_from_slice(&0u32.to_le_bytes()); // system_value_type
    bytes.extend_from_slice(&3u32.to_le_bytes()); // component_type (float32)
    bytes.extend_from_slice(&register.to_le_bytes());
    bytes.extend_from_slice(&u32::from_le_bytes([0xF, 0xF, 0, 0]).to_le_bytes()); // mask/rw/pad
    bytes.extend_from_slice(&stream.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes()); // min_precision

    bytes.extend_from_slice(semantic_name.as_bytes());
    bytes.push(0);

    bytes
}

fn build_dxbc(chunks: &[(FourCC, &[u8])]) -> Vec<u8> {
    let chunk_count = u32::try_from(chunks.len()).expect("too many chunks for test");
    let header_len = 4 + 16 + 4 + 4 + 4 + (chunks.len() * 4);

    let mut offsets = Vec::with_capacity(chunks.len());
    let mut cursor = header_len;
    for (_fourcc, data) in chunks {
        offsets.push(cursor as u32);
        cursor += 8 + data.len();
    }

    let total_size = cursor as u32;
    let mut bytes = Vec::with_capacity(cursor);

    bytes.extend_from_slice(b"DXBC");
    bytes.extend_from_slice(&[0u8; 16]); // checksum (ignored by parser)
    bytes.extend_from_slice(&1u32.to_le_bytes()); // reserved/unknown
    bytes.extend_from_slice(&total_size.to_le_bytes());
    bytes.extend_from_slice(&chunk_count.to_le_bytes());
    for off in offsets {
        bytes.extend_from_slice(&off.to_le_bytes());
    }

    for (fourcc, data) in chunks {
        bytes.extend_from_slice(&fourcc.0);
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(data);
    }

    assert_eq!(bytes.len(), total_size as usize);
    bytes
}

#[test]
fn parses_isgn_v0_signature_chunk() {
    let bytes = build_signature_chunk_v0_one_entry("POSITION", 0);
    let sig = parse_signature_chunk(FourCC(*b"ISGN"), &bytes).expect("signature parse should succeed");

    assert_eq!(sig.parameters.len(), 1);
    let p = &sig.parameters[0];
    assert_eq!(p.semantic_name, "POSITION");
    assert_eq!(p.semantic_index, 0);
    assert_eq!(p.register, 0);
    assert_eq!(p.system_value_type, 0);
    assert_eq!(p.component_type, 3);
    assert_eq!(p.mask, 0xF);
    assert_eq!(p.read_write_mask, 0xF);
    assert_eq!(p.stream, 0);
    assert_eq!(p.min_precision, 0);
}

#[test]
fn parses_isg1_v1_signature_chunk_and_preserves_stream() {
    let bytes = build_signature_chunk_v1_one_entry("POSITION", 0, 2);
    let sig = parse_signature_chunk(FourCC(*b"ISG1"), &bytes).expect("signature parse should succeed");

    assert_eq!(sig.parameters.len(), 1);
    let p = &sig.parameters[0];
    assert_eq!(p.semantic_name, "POSITION");
    assert_eq!(p.register, 0);
    assert_eq!(p.mask, 0xF);
    assert_eq!(p.read_write_mask, 0xF);
    assert_eq!(p.stream, 2);
}

#[test]
fn parse_signatures_prefers_v1_chunk_id_when_both_exist() {
    let isgn = build_signature_chunk_v0_one_entry("V0", 0);
    let isg1 = build_signature_chunk_v1_one_entry("V1", 1, 0);

    let dxbc_bytes = build_dxbc(&[
        (FourCC(*b"ISGN"), &isgn),
        (FourCC(*b"ISG1"), &isg1),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse should succeed");

    let sigs = parse_signatures(&dxbc).expect("signature parse should succeed");
    let sig = sigs.isgn.expect("expected input signature");

    assert_eq!(sig.parameters.len(), 1);
    assert_eq!(sig.parameters[0].semantic_name, "V1");
    assert_eq!(sig.parameters[0].register, 1);
}

