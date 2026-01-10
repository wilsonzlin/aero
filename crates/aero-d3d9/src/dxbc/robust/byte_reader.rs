use std::fmt;

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ByteReaderError {
    #[error("unexpected end of input at offset {offset} (needed {needed} bytes, remaining {remaining} bytes)")]
    UnexpectedEof {
        offset: usize,
        needed: usize,
        remaining: usize,
    },
    #[error("offset {offset} is out of bounds (len {len})")]
    OffsetOutOfBounds { offset: usize, len: usize },
    #[error("string at offset {offset} is not valid UTF-8")]
    InvalidUtf8 { offset: usize },
    #[error("c-string starting at offset {offset} is missing a null terminator")]
    UnterminatedCString { offset: usize },
}

#[derive(Clone, Copy)]
pub struct ByteReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> ByteReader<'a> {
    #[allow(dead_code)]
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    #[allow(dead_code)]
    pub fn position(&self) -> usize {
        self.pos
    }

    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    pub fn fork(&self, pos: usize) -> Result<Self, ByteReaderError> {
        if pos > self.data.len() {
            return Err(ByteReaderError::OffsetOutOfBounds {
                offset: pos,
                len: self.data.len(),
            });
        }
        Ok(Self {
            data: self.data,
            pos,
        })
    }

    #[allow(dead_code)]
    pub fn seek(&mut self, pos: usize) -> Result<(), ByteReaderError> {
        if pos > self.data.len() {
            return Err(ByteReaderError::OffsetOutOfBounds {
                offset: pos,
                len: self.data.len(),
            });
        }
        self.pos = pos;
        Ok(())
    }

    pub fn slice(&self, pos: usize, len: usize) -> Result<&'a [u8], ByteReaderError> {
        let end = pos
            .checked_add(len)
            .ok_or(ByteReaderError::OffsetOutOfBounds {
                offset: usize::MAX,
                len: self.data.len(),
            })?;
        if end > self.data.len() {
            return Err(ByteReaderError::UnexpectedEof {
                offset: pos,
                needed: len,
                remaining: self.data.len().saturating_sub(pos),
            });
        }
        Ok(&self.data[pos..end])
    }

    pub fn read_bytes(&mut self, len: usize) -> Result<&'a [u8], ByteReaderError> {
        let start = self.pos;
        let bytes = self.slice(start, len)?;
        self.pos = start + len;
        Ok(bytes)
    }

    pub fn read_u8(&mut self) -> Result<u8, ByteReaderError> {
        Ok(self.read_bytes(1)?[0])
    }

    pub fn read_u16_le(&mut self) -> Result<u16, ByteReaderError> {
        let bytes = self.read_bytes(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    pub fn read_u32_le(&mut self) -> Result<u32, ByteReaderError> {
        let bytes = self.read_bytes(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub fn read_fourcc(&mut self) -> Result<[u8; 4], ByteReaderError> {
        let bytes = self.read_bytes(4)?;
        Ok([bytes[0], bytes[1], bytes[2], bytes[3]])
    }

    pub fn read_cstring_at(&self, offset: usize) -> Result<&'a str, ByteReaderError> {
        if offset >= self.data.len() {
            return Err(ByteReaderError::OffsetOutOfBounds {
                offset,
                len: self.data.len(),
            });
        }

        let haystack = &self.data[offset..];
        let nul = haystack
            .iter()
            .position(|&b| b == 0)
            .ok_or(ByteReaderError::UnterminatedCString { offset })?;
        let bytes = &haystack[..nul];
        std::str::from_utf8(bytes).map_err(|_| ByteReaderError::InvalidUtf8 { offset })
    }
}

impl fmt::Debug for ByteReader<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ByteReader")
            .field("len", &self.data.len())
            .field("pos", &self.pos)
            .finish()
    }
}
