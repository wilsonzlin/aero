use iced_x86::Code;
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum EmuException {
    #[error("#DE divide error")]
    DivideError,

    #[error("#UD invalid opcode")]
    InvalidOpcode,

    #[error("unimplemented instruction: {0:?}")]
    Unimplemented(Code),

    #[error("memory access out of bounds at physical address {0:#x}")]
    MemOutOfBounds(u64),

    #[error("halted")]
    Halted,
}
