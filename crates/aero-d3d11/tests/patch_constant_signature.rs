use aero_d3d11::{parse_signatures, DxbcFile, FourCC};

const FOURCC_PCSG: FourCC = FourCC(*b"PCSG");
const FOURCC_PCG1: FourCC = FourCC(*b"PCG1");

// Explicit argument lists mirror on-disk signature entry layout fields (keeps call sites readable).
#[allow(clippy::too_many_arguments)]
fn build_signature_chunk_v0_one_entry(
    semantic_name: &str,
    semantic_index: u32,
    system_value_type: u32,
    component_type: u32,
    register: u32,
    mask: u8,
    read_write_mask: u8,
    stream: u8,
) -> Vec<u8> {
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
    bytes.extend_from_slice(&semantic_index.to_le_bytes());
    bytes.extend_from_slice(&system_value_type.to_le_bytes());
    bytes.extend_from_slice(&component_type.to_le_bytes());
    bytes.extend_from_slice(&register.to_le_bytes());
    bytes.push(mask);
    bytes.push(read_write_mask);
    bytes.push(stream);
    bytes.push(0); // min_precision (ignored by aero-dxbc)

    bytes.extend_from_slice(semantic_name.as_bytes());
    bytes.push(0);

    bytes
}

// Explicit argument lists mirror on-disk signature entry layout fields (keeps call sites readable).
#[allow(clippy::too_many_arguments)]
fn build_signature_chunk_v1_one_entry(
    semantic_name: &str,
    semantic_index: u32,
    system_value_type: u32,
    component_type: u32,
    register: u32,
    mask: u8,
    read_write_mask: u8,
    stream: u32,
) -> Vec<u8> {
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
    bytes.extend_from_slice(&semantic_index.to_le_bytes());
    bytes.extend_from_slice(&system_value_type.to_le_bytes());
    bytes.extend_from_slice(&component_type.to_le_bytes());
    bytes.extend_from_slice(&register.to_le_bytes());
    bytes.push(mask);
    bytes.push(read_write_mask);
    bytes.extend_from_slice(&[0u8; 2]); // padding
    bytes.extend_from_slice(&stream.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes()); // min_precision (ignored by aero-dxbc)

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
fn parses_pcsg_patch_constant_signature_chunk() {
    let pcsg_bytes =
        build_signature_chunk_v0_one_entry("TESSFACTOR", 1, 7, 3, 9, 0b0011, 0b0001, 0);

    let dxbc_bytes = build_dxbc(&[(FOURCC_PCSG, &pcsg_bytes)]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse should succeed");

    let sigs = parse_signatures(&dxbc).expect("signature parse should succeed");
    let sig = sigs.pcsg.expect("expected patch-constant signature");

    assert_eq!(sig.parameters.len(), 1);
    let p = &sig.parameters[0];
    assert_eq!(p.semantic_name, "TESSFACTOR");
    assert_eq!(p.semantic_index, 1);
    assert_eq!(p.system_value_type, 7);
    assert_eq!(p.component_type, 3);
    assert_eq!(p.register, 9);
    assert_eq!(p.mask, 0b0011);
    assert_eq!(p.read_write_mask, 0b0001);
}

#[test]
fn parses_pcg1_patch_constant_signature_chunk() {
    let pcg1_bytes =
        build_signature_chunk_v1_one_entry("PATCH_CONST", 2, 11, 1, 4, 0b1111, 0b0111, 2);

    let dxbc_bytes = build_dxbc(&[(FOURCC_PCG1, &pcg1_bytes)]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse should succeed");

    let sigs = parse_signatures(&dxbc).expect("signature parse should succeed");
    let sig = sigs.pcsg.expect("expected patch-constant signature");

    assert_eq!(sig.parameters.len(), 1);
    let p = &sig.parameters[0];
    assert_eq!(p.semantic_name, "PATCH_CONST");
    assert_eq!(p.semantic_index, 2);
    assert_eq!(p.system_value_type, 11);
    assert_eq!(p.component_type, 1);
    assert_eq!(p.register, 4);
    assert_eq!(p.mask, 0b1111);
    assert_eq!(p.read_write_mask, 0b0111);
    assert_eq!(p.stream, 2);
}

#[test]
fn parse_signatures_prefers_pcg1_over_pcsg_when_both_present() {
    let pcsg_bytes = build_signature_chunk_v0_one_entry("OLD", 0, 0, 3, 0, 0b1111, 0b1111, 0);
    let pcg1_bytes = build_signature_chunk_v1_one_entry("NEW", 0, 0, 3, 1, 0b0011, 0b0011, 0);

    let dxbc_bytes = build_dxbc(&[(FOURCC_PCSG, &pcsg_bytes), (FOURCC_PCG1, &pcg1_bytes)]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse should succeed");

    let sigs = parse_signatures(&dxbc).expect("signature parse should succeed");
    let sig = sigs.pcsg.expect("expected patch-constant signature");

    assert_eq!(sig.parameters.len(), 1);
    assert_eq!(sig.parameters[0].semantic_name, "NEW");
    assert_eq!(sig.parameters[0].register, 1);
}

#[test]
fn parse_signatures_falls_back_to_pcsg_when_all_pcg1_chunks_are_malformed() {
    // `parse_signatures` should skip malformed preferred chunks and fall back to the older `PCSG`
    // variant when possible (mirroring ISG1 -> ISGN behavior).
    let mut bad_pcg1 = Vec::new();
    bad_pcg1.extend_from_slice(&1u32.to_le_bytes()); // param_count
    bad_pcg1.extend_from_slice(&4u32.to_le_bytes()); // param_offset (invalid; points into header)

    let good_pcsg = build_signature_chunk_v0_one_entry("FALLBACK", 0, 0, 3, 7, 0b1111, 0b1111, 0);

    let dxbc_bytes = build_dxbc(&[(FOURCC_PCG1, &bad_pcg1), (FOURCC_PCSG, &good_pcsg)]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse should succeed");

    let sigs = parse_signatures(&dxbc).expect("signature parse should succeed");
    let sig = sigs.pcsg.expect("expected patch-constant signature");

    assert_eq!(sig.parameters.len(), 1);
    assert_eq!(sig.parameters[0].semantic_name, "FALLBACK");
    assert_eq!(sig.parameters[0].register, 7);
}

#[test]
fn parse_signatures_skips_malformed_duplicate_pcg1_chunks() {
    // If multiple `PCG1` chunks exist, choose the first one that parses successfully.
    let mut bad_pcg1 = Vec::new();
    bad_pcg1.extend_from_slice(&1u32.to_le_bytes()); // param_count
    bad_pcg1.extend_from_slice(&4u32.to_le_bytes()); // param_offset (invalid)

    let good_pcg1 = build_signature_chunk_v1_one_entry("GOOD", 0, 0, 3, 3, 0b1111, 0b1111, 0);

    let dxbc_bytes = build_dxbc(&[(FOURCC_PCG1, &bad_pcg1), (FOURCC_PCG1, &good_pcg1)]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse should succeed");

    let sigs = parse_signatures(&dxbc).expect("signature parse should succeed");
    let sig = sigs.pcsg.expect("expected patch-constant signature");

    assert_eq!(sig.parameters.len(), 1);
    assert_eq!(sig.parameters[0].semantic_name, "GOOD");
    assert_eq!(sig.parameters[0].register, 3);
}
