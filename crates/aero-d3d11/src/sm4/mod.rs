pub mod decode;
pub mod opcode;
pub mod token_dump;
pub(crate) mod validate;

pub use decode::{decode_program, Sm4DecodeError};
pub(crate) use validate::scan_sm5_nonzero_gs_stream;

pub use aero_dxbc::sm4::{
    decode_version_token, ShaderModel, ShaderStage, Sm4Error, Sm4Program, Sm5Program, FOURCC_SHDR,
    FOURCC_SHEX,
};
