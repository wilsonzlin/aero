use aero_dxbc::sm4::{ShaderStage, Sm4Error, Sm4Program};
use aero_dxbc::{DxbcFile, FourCC};

fn build_dxbc(chunks: &[(FourCC, &[u8])]) -> Vec<u8> {
    let chunk_count = u32::try_from(chunks.len()).expect("too many chunks for test");
    let header_len = 4 + 16 + 4 + 4 + 4 + (chunks.len() * 4);

    // Compute chunk offsets.
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
fn sm4_parse_from_dxbc_smoke() {
    // Vertex shader model 4.0:
    // - program type = 1 (vertex) in bits 16..=31
    // - major = 4 in bits 4..=7
    // - minor = 0 in bits 0..=3
    let version: u32 = (1u32 << 16) | (4u32 << 4) | 0u32;
    let tokens = [version, 2u32]; // declared length includes version+len
    let mut shdr = Vec::new();
    for t in tokens {
        shdr.extend_from_slice(&t.to_le_bytes());
    }

    let bytes = build_dxbc(&[(FourCC(*b"SHDR"), &shdr)]);
    let dxbc = DxbcFile::parse(&bytes).expect("parse dxbc");
    let prog = Sm4Program::parse_from_dxbc(&dxbc).expect("parse sm4 tokens");
    assert_eq!(prog.stage, ShaderStage::Vertex);
    assert_eq!(prog.model.major, 4);
    assert_eq!(prog.model.minor, 0);
    assert_eq!(prog.tokens.len(), 2);
}

#[test]
fn sm4_missing_shader_chunk_is_error() {
    let bytes = build_dxbc(&[(FourCC(*b"JUNK"), &[1, 2, 3, 4])]);
    let dxbc = DxbcFile::parse(&bytes).expect("parse dxbc");
    let err = Sm4Program::parse_from_dxbc(&dxbc).unwrap_err();
    assert!(matches!(err, Sm4Error::MissingShaderChunk));
}

#[test]
fn sm4_declared_length_oob_is_error() {
    let version: u32 = (1u32 << 16) | (4u32 << 4) | 0u32;
    let tokens = [version, 100u32]; // absurd length
    let mut shdr = Vec::new();
    for t in tokens {
        shdr.extend_from_slice(&t.to_le_bytes());
    }

    let bytes = build_dxbc(&[(FourCC(*b"SHDR"), &shdr)]);
    let dxbc = DxbcFile::parse(&bytes).expect("parse dxbc");
    let err = Sm4Program::parse_from_dxbc(&dxbc).unwrap_err();
    assert!(matches!(err, Sm4Error::DeclaredLengthOutOfBounds { .. }));
}

