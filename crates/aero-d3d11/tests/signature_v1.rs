use aero_d3d11::{
    parse_signature_chunk, parse_signatures, DxbcFile, DxbcSignatureParameter, FourCC,
    SignatureError,
};
use aero_dxbc::test_utils as dxbc_test_utils;

const FOURCC_ISGN: FourCC = FourCC(*b"ISGN");
const FOURCC_ISG1: FourCC = FourCC(*b"ISG1");
const FOURCC_OSGN: FourCC = FourCC(*b"OSGN");
const FOURCC_OSG1: FourCC = FourCC(*b"OSG1");
const FOURCC_PSGN: FourCC = FourCC(*b"PSGN");
const FOURCC_PSG1: FourCC = FourCC(*b"PSG1");

fn build_dxbc(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
    dxbc_test_utils::build_container_owned(chunks)
}

fn sig_param(
    name: &str,
    index: u32,
    register: u32,
    mask: u8,
    stream: u8,
) -> DxbcSignatureParameter {
    DxbcSignatureParameter {
        semantic_name: name.to_owned(),
        semantic_index: index,
        system_value_type: 0,
        component_type: 0,
        register,
        mask,
        read_write_mask: mask,
        stream,
        min_precision: 0,
    }
}

fn build_signature_chunk_v0(params: &[DxbcSignatureParameter]) -> Vec<u8> {
    let entries: Vec<dxbc_test_utils::SignatureEntryDesc<'_>> = params
        .iter()
        .map(|p| dxbc_test_utils::SignatureEntryDesc {
            semantic_name: p.semantic_name.as_str(),
            semantic_index: p.semantic_index,
            system_value_type: p.system_value_type,
            component_type: p.component_type,
            register: p.register,
            mask: p.mask,
            read_write_mask: p.read_write_mask,
            stream: u32::from(p.stream),
        })
        .collect();

    let mut bytes = dxbc_test_utils::build_signature_chunk_v0(&entries);

    // Patch the v0 min-precision byte (stored in the last byte of the packed DWORD).
    let table_start = 8usize;
    let entry_size = 24usize;
    for (i, p) in params.iter().enumerate() {
        let base = table_start + i * entry_size;
        bytes[base + 23] = p.min_precision;
    }

    bytes
}

fn build_signature_chunk_v1(params: &[DxbcSignatureParameter]) -> Vec<u8> {
    let entries: Vec<dxbc_test_utils::SignatureEntryDesc<'_>> = params
        .iter()
        .map(|p| dxbc_test_utils::SignatureEntryDesc {
            semantic_name: p.semantic_name.as_str(),
            semantic_index: p.semantic_index,
            system_value_type: p.system_value_type,
            component_type: p.component_type,
            register: p.register,
            mask: p.mask,
            read_write_mask: p.read_write_mask,
            stream: u32::from(p.stream),
        })
        .collect();

    let mut bytes = dxbc_test_utils::build_signature_chunk_v1(&entries);

    // Patch v1 min-precision DWORDs so tests can verify they are ignored by the parser.
    let table_start = 8usize;
    let entry_size = 32usize;
    for (i, p) in params.iter().enumerate() {
        let base = table_start + i * entry_size;
        bytes[base + 28..base + 32].copy_from_slice(&(u32::from(p.min_precision)).to_le_bytes());
    }

    bytes
}

fn build_signature_chunk_v1_one_entry(stream: u32) -> Vec<u8> {
    dxbc_test_utils::build_signature_chunk_v1(&[dxbc_test_utils::SignatureEntryDesc {
        semantic_name: "POSITION",
        semantic_index: 0,
        system_value_type: 0,
        component_type: 0,
        register: 0,
        mask: 0xF,
        read_write_mask: 0x3,
        stream,
    }])
}

#[test]
fn parses_v1_signature_chunk_entries() {
    let mut params = vec![
        sig_param("POSITION", 0, 0, 0b0011, 0),
        sig_param("COLOR", 0, 1, 0b1111, 2),
    ];
    params[0].system_value_type = 7;
    params[0].component_type = 3;
    params[0].read_write_mask = 0b0001;
    // v1 layout stores min-precision as a full DWORD (ignored by aero-dxbc).
    params[0].min_precision = 7;

    let chunk_bytes = build_signature_chunk_v1(&params);
    let sig = parse_signature_chunk(FOURCC_ISG1, &chunk_bytes).expect("parse ISG1 signature");

    assert_eq!(sig.parameters.len(), 2);
    assert_eq!(sig.parameters[0].semantic_name, "POSITION");
    assert_eq!(sig.parameters[0].semantic_index, 0);
    assert_eq!(sig.parameters[0].register, 0);
    assert_eq!(sig.parameters[0].system_value_type, 7);
    assert_eq!(sig.parameters[0].component_type, 3);
    assert_eq!(sig.parameters[0].mask, 0b0011);
    assert_eq!(sig.parameters[0].read_write_mask, 0b0001);
    assert_eq!(sig.parameters[0].stream, 0);
    assert_eq!(sig.parameters[0].min_precision, 0);

    assert_eq!(sig.parameters[1].semantic_name, "COLOR");
    assert_eq!(sig.parameters[1].register, 1);
    assert_eq!(sig.parameters[1].mask, 0b1111);
    assert_eq!(sig.parameters[1].stream, 2);
}

#[test]
fn rejects_v1_stream_out_of_range() {
    let chunk_bytes = build_signature_chunk_v1_one_entry(256);
    let err = parse_signature_chunk(FOURCC_ISG1, &chunk_bytes).unwrap_err();

    assert!(matches!(
        err,
        SignatureError::MalformedChunk {
            fourcc: FOURCC_ISG1,
            reason
        } if reason.contains("does not fit in u8")
    ));
}

#[test]
fn parses_v0_layout_even_when_fourcc_is_isg1() {
    // Conversely, accept the legacy 24-byte entry layout even if the chunk ID ends with `1`.
    let params = vec![sig_param("POSITION", 0, 0, 0b1111, 2)];
    let chunk_bytes = build_signature_chunk_v0(&params);

    let sig = parse_signature_chunk(FOURCC_ISG1, &chunk_bytes).expect("parse ISG1 signature");
    assert_eq!(sig.parameters.len(), 1);
    assert_eq!(sig.parameters[0].semantic_name, "POSITION");
    assert_eq!(sig.parameters[0].stream, 2);
}

#[test]
fn parses_v1_layout_when_fourcc_is_unknown() {
    // When the FourCC suffix doesn't indicate a specific signature entry layout, the
    // aero-dxbc parser uses heuristics to choose between v0 and v1 encodings.
    let params = vec![sig_param("POSITION", 0, 0, 0b1111, 2)];
    let chunk_bytes = build_signature_chunk_v1(&params);

    let sig = parse_signature_chunk(FourCC(*b"XXXX"), &chunk_bytes).expect("parse signature");
    assert_eq!(sig.parameters.len(), 1);
    assert_eq!(sig.parameters[0].semantic_name, "POSITION");
    assert_eq!(sig.parameters[0].stream, 2);
}

#[test]
fn prefers_v1_variant_when_both_present() {
    let v0_params = vec![sig_param("OLD", 0, 7, 0b1111, 0)];
    let v1_params = vec![
        sig_param("POSITION", 0, 0, 0b0011, 0),
        sig_param("COLOR", 0, 1, 0b1111, 2),
    ];

    let dxbc_bytes = build_dxbc(&[
        (FOURCC_ISGN, build_signature_chunk_v0(&v0_params)),
        (FOURCC_ISG1, build_signature_chunk_v1(&v1_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let sigs = parse_signatures(&dxbc).expect("parse signatures");

    let isgn = sigs.isgn.expect("ISGN/ISG1 signature missing");
    assert_eq!(isgn.parameters.len(), 2);
    assert_eq!(isgn.parameters[0].semantic_name, "POSITION");
    assert_eq!(isgn.parameters[1].semantic_name, "COLOR");
}

#[test]
fn skips_malformed_duplicate_isg1_chunks() {
    // First chunk is malformed (`param_offset` points into the header). The second chunk is valid
    // and should be chosen.
    let mut bad_bytes = Vec::new();
    bad_bytes.extend_from_slice(&1u32.to_le_bytes()); // param_count
    bad_bytes.extend_from_slice(&4u32.to_le_bytes()); // param_offset (invalid)

    let good_params = vec![sig_param("POSITION", 0, 0, 0b1111, 0)];
    let good_bytes = build_signature_chunk_v1(&good_params);

    let dxbc_bytes = build_dxbc(&[(FOURCC_ISG1, bad_bytes), (FOURCC_ISG1, good_bytes)]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let sigs = parse_signatures(&dxbc).expect("parse signatures");

    let isgn = sigs.isgn.expect("ISG1 signature missing");
    assert_eq!(isgn.parameters.len(), 1);
    assert_eq!(isgn.parameters[0].semantic_name, "POSITION");
}

#[test]
fn falls_back_to_isgn_when_all_isg1_chunks_are_malformed() {
    let mut bad_bytes = Vec::new();
    bad_bytes.extend_from_slice(&1u32.to_le_bytes()); // param_count
    bad_bytes.extend_from_slice(&4u32.to_le_bytes()); // param_offset (invalid)

    let good_v0_params = vec![sig_param("POSITION", 0, 0, 0b1111, 0)];
    let good_v0_bytes = build_signature_chunk_v0(&good_v0_params);

    let dxbc_bytes = build_dxbc(&[(FOURCC_ISG1, bad_bytes), (FOURCC_ISGN, good_v0_bytes)]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let sigs = parse_signatures(&dxbc).expect("parse signatures");

    let isgn = sigs.isgn.expect("ISGN signature missing");
    assert_eq!(isgn.parameters.len(), 1);
    assert_eq!(isgn.parameters[0].semantic_name, "POSITION");
}

#[test]
fn prefers_osg1_variant_when_both_present() {
    let v0_params = vec![sig_param("OLD_OUT", 0, 7, 0b1111, 0)];
    let v1_params = vec![sig_param("NEW_OUT", 0, 1, 0b0011, 0)];

    let dxbc_bytes = build_dxbc(&[
        (FOURCC_OSGN, build_signature_chunk_v0(&v0_params)),
        (FOURCC_OSG1, build_signature_chunk_v1(&v1_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let sigs = parse_signatures(&dxbc).expect("parse signatures");

    let osgn = sigs.osgn.expect("OSGN/OSG1 signature missing");
    assert_eq!(osgn.parameters.len(), 1);
    assert_eq!(osgn.parameters[0].semantic_name, "NEW_OUT");
    assert_eq!(osgn.parameters[0].register, 1);
}

#[test]
fn prefers_psg1_variant_when_both_present() {
    let v0_params = vec![sig_param("OLD_PATCH", 0, 7, 0b1111, 0)];
    let v1_params = vec![sig_param("NEW_PATCH", 0, 1, 0b0011, 0)];

    let dxbc_bytes = build_dxbc(&[
        (FOURCC_PSGN, build_signature_chunk_v0(&v0_params)),
        (FOURCC_PSG1, build_signature_chunk_v1(&v1_params)),
    ]);
    let dxbc = DxbcFile::parse(&dxbc_bytes).expect("DXBC parse");
    let sigs = parse_signatures(&dxbc).expect("parse signatures");

    let psgn = sigs.psgn.expect("PSGN/PSG1 signature missing");
    assert_eq!(psgn.parameters.len(), 1);
    assert_eq!(psgn.parameters[0].semantic_name, "NEW_PATCH");
    assert_eq!(psgn.parameters[0].register, 1);
}
