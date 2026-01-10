use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShaderParseError {
    Empty,
    InvalidByteLength {
        len: usize,
    },
    Dxbc(aero_dxbc::DxbcError),
    DxbcMissingShaderChunk,
    Truncated {
        at_token: usize,
    },
    InvalidVersionToken {
        token: u32,
    },
    TruncatedInstruction {
        opcode: u16,
        at_token: usize,
        needed_tokens: usize,
        remaining_tokens: usize,
    },
}

impl From<aero_dxbc::DxbcError> for ShaderParseError {
    fn from(value: aero_dxbc::DxbcError) -> Self {
        ShaderParseError::Dxbc(value)
    }
}

impl fmt::Display for ShaderParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ShaderParseError::Empty => write!(f, "empty shader blob"),
            ShaderParseError::InvalidByteLength { len } => {
                write!(f, "shader blob length {len} is not a multiple of 4")
            }
            ShaderParseError::Dxbc(e) => write!(f, "DXBC parse error: {e}"),
            ShaderParseError::DxbcMissingShaderChunk => {
                write!(f, "DXBC container missing SHEX/SHDR shader chunk")
            }
            ShaderParseError::Truncated { at_token } => write!(f, "truncated at token {at_token}"),
            ShaderParseError::InvalidVersionToken { token } => {
                write!(f, "invalid version token 0x{token:08x}")
            }
            ShaderParseError::TruncatedInstruction {
                opcode,
                at_token,
                needed_tokens,
                remaining_tokens,
            } => write!(
                f,
                "truncated instruction opcode 0x{opcode:04x} at token {at_token} (needed {needed_tokens} tokens, had {remaining_tokens})"
            ),
        }
    }
}

impl std::error::Error for ShaderParseError {}
