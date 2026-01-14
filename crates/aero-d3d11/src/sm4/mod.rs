pub mod decode;
pub mod opcode;
pub mod token_dump;

pub use decode::{decode_program, Sm4DecodeError};

pub use aero_dxbc::sm4::{
    decode_version_token, ShaderModel, ShaderStage, Sm4Error, Sm4Program, Sm5Program, FOURCC_SHDR,
    FOURCC_SHEX,
};
