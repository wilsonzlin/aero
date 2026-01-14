use crate::sm4::{ShaderModel, ShaderStage, Sm4Error, Sm4Program};
use crate::test_utils as dxbc_test_utils;
use crate::{DxbcFile, FourCC};

fn build_dxbc(chunks: &[(FourCC, &[u8])]) -> Vec<u8> {
    dxbc_test_utils::build_container(chunks)
}

fn tokens_to_bytes(tokens: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(tokens.len() * 4);
    for &t in tokens {
        bytes.extend_from_slice(&t.to_le_bytes());
    }
    bytes
}

fn make_program_header(stage_type: u16, model_major: u8, model_minor: u8, declared_len: u32) -> [u32; 2] {
    let version =
        ((stage_type as u32) << 16) | ((model_major as u32) << 4) | (model_minor as u32);
    [version, declared_len]
}

#[test]
fn parses_shdr_and_decodes_stage_and_model() {
    // Vertex shader model 4.0.
    let header = make_program_header(1, 4, 0, 2);
    let shdr = tokens_to_bytes(&header);
    let bytes = build_dxbc(&[(FourCC(*b"SHDR"), &shdr)]);

    let dxbc = DxbcFile::parse(&bytes).expect("DXBC parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");

    assert_eq!(program.stage, ShaderStage::Vertex);
    assert_eq!(program.model, ShaderModel { major: 4, minor: 0 });
    assert_eq!(program.tokens.len(), 2);
}

#[test]
fn parse_from_dxbc_prefers_shex_over_shdr() {
    let shdr_header = make_program_header(1, 4, 0, 2);
    let shex_header = make_program_header(0, 5, 0, 2);
    let shdr = tokens_to_bytes(&shdr_header);
    let shex = tokens_to_bytes(&shex_header);
    let bytes = build_dxbc(&[(FourCC(*b"SHDR"), &shdr), (FourCC(*b"SHEX"), &shex)]);

    let dxbc = DxbcFile::parse(&bytes).expect("DXBC parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM parse");

    assert_eq!(program.stage, ShaderStage::Pixel);
    assert_eq!(program.model.major, 5);
}

#[test]
fn rejects_misaligned_token_stream() {
    let bytes = build_dxbc(&[(FourCC(*b"SHDR"), &[0u8; 5])]);
    let dxbc = DxbcFile::parse(&bytes).expect("DXBC parse");

    let err = Sm4Program::parse_from_dxbc(&dxbc).unwrap_err();
    assert!(matches!(err, Sm4Error::MisalignedTokens { len: 5 }));
}

#[test]
fn rejects_too_short_token_stream() {
    // Only 1 DWORD.
    let bytes = build_dxbc(&[(FourCC(*b"SHDR"), &[0u8; 4])]);
    let dxbc = DxbcFile::parse(&bytes).expect("DXBC parse");

    let err = Sm4Program::parse_from_dxbc(&dxbc).unwrap_err();
    assert!(matches!(err, Sm4Error::TooShort { dwords: 1 }));
}

#[test]
fn rejects_declared_length_out_of_bounds() {
    // Two DWORDs provided, but declared length is 3.
    let header = make_program_header(0, 4, 0, 3);
    let shdr = tokens_to_bytes(&header);
    let bytes = build_dxbc(&[(FourCC(*b"SHDR"), &shdr)]);
    let dxbc = DxbcFile::parse(&bytes).expect("DXBC parse");

    let err = Sm4Program::parse_from_dxbc(&dxbc).unwrap_err();
    assert!(matches!(
        err,
        Sm4Error::DeclaredLengthOutOfBounds {
            declared: 3,
            available: 2
        }
    ));
}

#[test]
fn declared_length_too_small_is_error() {
    // Two DWORDs provided, but declared length is 1 (invalid; must include version+len).
    let header = make_program_header(0, 4, 0, 1);
    let shdr = tokens_to_bytes(&header);
    let bytes = build_dxbc(&[(FourCC(*b"SHDR"), &shdr)]);
    let dxbc = DxbcFile::parse(&bytes).expect("DXBC parse");

    let err = Sm4Program::parse_from_dxbc(&dxbc).unwrap_err();
    assert!(matches!(err, Sm4Error::DeclaredLengthTooSmall { declared: 1 }));
}

#[test]
fn declared_length_truncates_trailing_bytes() {
    // Provide extra DWORDs beyond the declared length; they should be ignored.
    let header = make_program_header(1, 4, 0, 2);
    let mut toks = Vec::from(header);
    toks.push(0xDEAD_BEEFu32);
    toks.push(0x1234_5678u32);
    let shdr = tokens_to_bytes(&toks);

    let bytes = build_dxbc(&[(FourCC(*b"SHDR"), &shdr)]);
    let dxbc = DxbcFile::parse(&bytes).expect("DXBC parse");
    let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse");

    assert_eq!(program.tokens.len(), 2);
    assert_eq!(program.tokens[0], header[0]);
    assert_eq!(program.tokens[1], header[1]);
}

#[test]
fn missing_shader_chunk_is_error() {
    let bytes = build_dxbc(&[(FourCC(*b"JUNK"), &[1, 2, 3, 4])]);
    let dxbc = DxbcFile::parse(&bytes).expect("DXBC parse");

    let err = Sm4Program::parse_from_dxbc(&dxbc).unwrap_err();
    assert!(matches!(err, Sm4Error::MissingShaderChunk));
}
