use aero_d3d11::{parse_signatures, DxbcFile, FourCC};
use aero_dxbc::test_utils as dxbc_test_utils;

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
    let entry = dxbc_test_utils::SignatureEntryDesc {
        semantic_name,
        semantic_index,
        system_value_type,
        component_type,
        register,
        mask,
        read_write_mask,
        stream: u32::from(stream),
        min_precision: 0,
    };
    dxbc_test_utils::build_signature_chunk_v0(&[entry])
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
    let entry = dxbc_test_utils::SignatureEntryDesc {
        semantic_name,
        semantic_index,
        system_value_type,
        component_type,
        register,
        mask,
        read_write_mask,
        stream,
        min_precision: 0,
    };
    dxbc_test_utils::build_signature_chunk_v1(&[entry])
}

fn build_dxbc(chunks: &[(FourCC, &[u8])]) -> Vec<u8> {
    dxbc_test_utils::build_container(chunks)
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
