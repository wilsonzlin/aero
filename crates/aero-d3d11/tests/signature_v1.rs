use aero_d3d11::{
    parse_signature_chunk, parse_signatures, DxbcFile, DxbcSignatureParameter, FourCC,
};

const FOURCC_ISGN: FourCC = FourCC(*b"ISGN");
const FOURCC_ISG1: FourCC = FourCC(*b"ISG1");

fn build_dxbc(chunks: &[(FourCC, Vec<u8>)]) -> Vec<u8> {
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
    // Header: param_count + param_offset.
    let param_count = u32::try_from(params.len()).expect("too many signature params");
    let header_len = 8usize;
    let entry_size = 24usize;
    let table_len = params.len() * entry_size;

    // Strings appended after table.
    let mut strings = Vec::<u8>::new();
    let mut name_offsets = Vec::<u32>::with_capacity(params.len());
    for p in params {
        name_offsets.push((header_len + table_len + strings.len()) as u32);
        strings.extend_from_slice(p.semantic_name.as_bytes());
        strings.push(0);
    }

    let mut bytes = Vec::with_capacity(header_len + table_len + strings.len());
    bytes.extend_from_slice(&param_count.to_le_bytes());
    bytes.extend_from_slice(&(header_len as u32).to_le_bytes());

    for (p, &name_off) in params.iter().zip(name_offsets.iter()) {
        bytes.extend_from_slice(&name_off.to_le_bytes());
        bytes.extend_from_slice(&p.semantic_index.to_le_bytes());
        bytes.extend_from_slice(&p.system_value_type.to_le_bytes());
        bytes.extend_from_slice(&p.component_type.to_le_bytes());
        bytes.extend_from_slice(&p.register.to_le_bytes());
        bytes.push(p.mask);
        bytes.push(p.read_write_mask);
        bytes.push(p.stream);
        bytes.push(p.min_precision);
    }
    bytes.extend_from_slice(&strings);
    bytes
}

fn build_signature_chunk_v1(params: &[DxbcSignatureParameter]) -> Vec<u8> {
    // Header: param_count + param_offset.
    let param_count = u32::try_from(params.len()).expect("too many signature params");
    let header_len = 8usize;
    let entry_size = 32usize;
    let table_len = params.len() * entry_size;

    // Strings appended after table.
    let mut strings = Vec::<u8>::new();
    let mut name_offsets = Vec::<u32>::with_capacity(params.len());
    for p in params {
        name_offsets.push((header_len + table_len + strings.len()) as u32);
        strings.extend_from_slice(p.semantic_name.as_bytes());
        strings.push(0);
    }

    let mut bytes = Vec::with_capacity(header_len + table_len + strings.len());
    bytes.extend_from_slice(&param_count.to_le_bytes());
    bytes.extend_from_slice(&(header_len as u32).to_le_bytes());

    for (p, &name_off) in params.iter().zip(name_offsets.iter()) {
        bytes.extend_from_slice(&name_off.to_le_bytes());
        bytes.extend_from_slice(&p.semantic_index.to_le_bytes());
        bytes.extend_from_slice(&p.system_value_type.to_le_bytes());
        bytes.extend_from_slice(&p.component_type.to_le_bytes());
        bytes.extend_from_slice(&p.register.to_le_bytes());
        bytes.push(p.mask);
        bytes.push(p.read_write_mask);
        bytes.extend_from_slice(&[0u8; 2]); // reserved/padding
        bytes.extend_from_slice(&(p.stream as u32).to_le_bytes());
        bytes.extend_from_slice(&(p.min_precision as u32).to_le_bytes());
    }
    bytes.extend_from_slice(&strings);
    bytes
}

#[test]
fn parses_v1_signature_chunk_entries() {
    let params = vec![
        sig_param("POSITION", 0, 0, 0b0011, 0),
        sig_param("COLOR", 0, 1, 0b1111, 2),
    ];
    let chunk_bytes = build_signature_chunk_v1(&params);
    let sig = parse_signature_chunk(FOURCC_ISG1, &chunk_bytes).expect("parse ISG1 signature");

    assert_eq!(sig.parameters.len(), 2);
    assert_eq!(sig.parameters[0].semantic_name, "POSITION");
    assert_eq!(sig.parameters[0].semantic_index, 0);
    assert_eq!(sig.parameters[0].register, 0);
    assert_eq!(sig.parameters[0].mask, 0b0011);
    assert_eq!(sig.parameters[0].stream, 0);

    assert_eq!(sig.parameters[1].semantic_name, "COLOR");
    assert_eq!(sig.parameters[1].register, 1);
    assert_eq!(sig.parameters[1].mask, 0b1111);
    assert_eq!(sig.parameters[1].stream, 2);
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
