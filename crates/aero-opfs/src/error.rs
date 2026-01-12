use core::fmt;

pub type DiskResult<T> = core::result::Result<T, DiskError>;

#[derive(Debug)]
pub enum DiskError {
    NotSupported(String),
    QuotaExceeded,
    InUse,
    InvalidState(String),
    OutOfBounds,
    InvalidBufferLength,
    CorruptImage(&'static str),
    Unsupported(&'static str),
    Io(String),
}

impl fmt::Display for DiskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotSupported(msg) => write!(f, "not supported: {msg}"),
            Self::QuotaExceeded => write!(f, "quota exceeded"),
            Self::InUse => write!(f, "resource in use"),
            Self::InvalidState(msg) => write!(f, "invalid state: {msg}"),
            Self::OutOfBounds => write!(f, "out of bounds"),
            Self::InvalidBufferLength => write!(f, "invalid buffer length"),
            Self::CorruptImage(msg) => write!(f, "corrupt image: {msg}"),
            Self::Unsupported(msg) => write!(f, "unsupported: {msg}"),
            Self::Io(msg) => write!(f, "io error: {msg}"),
        }
    }
}

impl std::error::Error for DiskError {}

