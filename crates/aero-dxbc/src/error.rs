use core::fmt;

/// An error returned when parsing a `DXBC` container fails.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DxbcError {
    /// The container header is malformed (e.g. truncated or has an invalid
    /// signature).
    MalformedHeader {
        /// Human-friendly context describing what went wrong.
        context: String,
    },
    /// The chunk offset table is malformed.
    MalformedOffsets {
        /// Human-friendly context describing what went wrong.
        context: String,
    },
    /// A chunk offset/size points outside of the declared container bounds.
    OutOfBounds {
        /// Human-friendly context describing what went wrong.
        context: String,
    },
    /// A chunk payload is malformed (e.g. truncated or internally inconsistent).
    InvalidChunk {
        /// Human-friendly context describing what went wrong.
        context: String,
    },
}

impl DxbcError {
    pub(crate) fn malformed_header(context: impl Into<String>) -> Self {
        Self::MalformedHeader {
            context: context.into(),
        }
    }

    pub(crate) fn malformed_offsets(context: impl Into<String>) -> Self {
        Self::MalformedOffsets {
            context: context.into(),
        }
    }

    pub(crate) fn out_of_bounds(context: impl Into<String>) -> Self {
        Self::OutOfBounds {
            context: context.into(),
        }
    }

    pub(crate) fn invalid_chunk(context: impl Into<String>) -> Self {
        Self::InvalidChunk {
            context: context.into(),
        }
    }

    /// Returns the human-friendly context associated with this error.
    pub fn context(&self) -> &str {
        match self {
            Self::MalformedHeader { context }
            | Self::MalformedOffsets { context }
            | Self::OutOfBounds { context }
            | Self::InvalidChunk { context } => context,
        }
    }
}

impl fmt::Display for DxbcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MalformedHeader { context } => write!(f, "malformed DXBC header: {context}"),
            Self::MalformedOffsets { context } => {
                write!(f, "malformed DXBC chunk offsets: {context}")
            }
            Self::OutOfBounds { context } => write!(f, "DXBC out of bounds: {context}"),
            Self::InvalidChunk { context } => write!(f, "invalid DXBC chunk: {context}"),
        }
    }
}

impl std::error::Error for DxbcError {}
