use aero_d3d11::{parse_signature_chunk, parse_signatures, DxbcFile, FourCC};
use aero_dxbc::test_utils as dxbc_test_utils;

fn build_signature_chunk_v0_one_entry(semantic_name: &str, register: u32) -> Vec<u8> {
    dxbc_test_utils::build_signature_chunk_v0(&[dxbc_test_utils::SignatureEntryDesc {
        semantic_name,
        semantic_index: 0,
        system_value_type: 0,
        component_type: 3, // float32
        register,
        mask: 0xF,
        read_write_mask: 0xF,
        stream: 0,
        min_precision: 0,
    }])
}

fn build_signature_chunk_v1_one_entry(semantic_name: &str, register: u32, stream: u32) -> Vec<u8> {
    dxbc_test_utils::build_signature_chunk_v1(&[dxbc_test_utils::SignatureEntryDesc {
        semantic_name,
        semantic_index: 0,
        system_value_type: 0,
        component_type: 3, // float32
        register,
        mask: 0xF,
        read_write_mask: 0xF,
        stream,
        min_precision: 0,
    }])
}

fn build_dxbc(chunks: &[(FourCC, &[u8])]) -> Vec<u8> {
    dxbc_test_utils::build_container(chunks)
}

#[test]
fn parses_isgn_v0_signature_chunk() {
    let bytes = build_signature_chunk_v0_one_entry("POSITION", 0);
    let sig =
        parse_signature_chunk(FourCC(*b"ISGN"), &bytes).expect("signature parse should succeed");

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
    let sig =
        parse_signature_chunk(FourCC(*b"ISG1"), &bytes).expect("signature parse should succeed");

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

    let dxbc_bytes = build_dxbc(&[(FourCC(*b"ISGN"), &isgn), (FourCC(*b"ISG1"), &isg1)]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse should succeed");

    let sigs = parse_signatures(&dxbc).expect("signature parse should succeed");
    let sig = sigs.isgn.expect("expected input signature");

    assert_eq!(sig.parameters.len(), 1);
    assert_eq!(sig.parameters[0].semantic_name, "V1");
    assert_eq!(sig.parameters[0].register, 1);
}

#[test]
fn parse_signatures_prefers_pcg1_when_both_exist() {
    let pcsg = build_signature_chunk_v0_one_entry("V0", 0);
    let pcg1 = build_signature_chunk_v1_one_entry("V1", 1, 0);

    let dxbc_bytes = build_dxbc(&[(FourCC(*b"PCSG"), &pcsg), (FourCC(*b"PCG1"), &pcg1)]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse should succeed");

    let sigs = parse_signatures(&dxbc).expect("signature parse should succeed");
    let sig = sigs.pcsg.expect("expected patch-constant signature");

    assert_eq!(sig.parameters.len(), 1);
    assert_eq!(sig.parameters[0].semantic_name, "V1");
    assert_eq!(sig.parameters[0].register, 1);
}

#[test]
fn parse_signatures_parses_pcsg_patch_constant_signature_chunk() {
    // Minimal signature chunk: zero entries, param_offset points past header.
    let pcsg = dxbc_test_utils::build_signature_chunk_v0(&[]);

    let dxbc_bytes = build_dxbc(&[(FourCC(*b"PCSG"), &pcsg)]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse should succeed");

    let sigs = parse_signatures(&dxbc).expect("signature parse should succeed");
    assert!(sigs.pcsg.is_some(), "expected pcsg signature to be parsed");
}

#[test]
fn parse_signatures_malformed_pcsg_mentions_fourcc() {
    // Truncated signature chunk payload (must be >= 8 bytes).
    let pcsg = [0u8, 0, 0, 0];

    let dxbc_bytes = build_dxbc(&[(FourCC(*b"PCSG"), &pcsg)]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse should succeed");

    let err = parse_signatures(&dxbc).expect_err("expected malformed PCSG to error");
    let msg = err.to_string();
    assert!(
        msg.contains("PCSG"),
        "expected error message to mention PCSG FourCC, got: {msg}"
    );
}
