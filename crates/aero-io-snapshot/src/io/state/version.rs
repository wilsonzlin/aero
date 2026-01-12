use core::fmt;
use std::collections::BTreeMap;

pub type SnapshotResult<T> = Result<T, SnapshotError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotError {
    UnexpectedEof,
    InvalidMagic,
    UnsupportedFormatVersion {
        found: SnapshotVersion,
        supported: SnapshotVersion,
    },
    DeviceIdMismatch {
        expected: [u8; 4],
        found: [u8; 4],
    },
    UnsupportedDeviceMajorVersion {
        found: u16,
        supported: u16,
    },
    DuplicateFieldTag(u16),
    InvalidFieldEncoding(&'static str),
    OutOfMemory,
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SnapshotError::UnexpectedEof => write!(f, "snapshot truncated"),
            SnapshotError::InvalidMagic => write!(f, "invalid snapshot magic"),
            SnapshotError::UnsupportedFormatVersion { found, supported } => write!(
                f,
                "unsupported snapshot format version {} (supported {})",
                found, supported
            ),
            SnapshotError::DeviceIdMismatch { expected, found } => write!(
                f,
                "snapshot device id mismatch (expected {:?}, found {:?})",
                expected, found
            ),
            SnapshotError::UnsupportedDeviceMajorVersion { found, supported } => write!(
                f,
                "unsupported device snapshot major version {} (supported {})",
                found, supported
            ),
            SnapshotError::DuplicateFieldTag(tag) => write!(f, "duplicate field tag {}", tag),
            SnapshotError::InvalidFieldEncoding(msg) => {
                write!(f, "invalid field encoding: {}", msg)
            }
            SnapshotError::OutOfMemory => write!(f, "out of memory"),
        }
    }
}

impl std::error::Error for SnapshotError {}

/// Major/minor version pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SnapshotVersion {
    pub major: u16,
    pub minor: u16,
}

impl SnapshotVersion {
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }
}

impl fmt::Display for SnapshotVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// Snapshot header preceding TLV fields.
///
/// Encoding (little-endian):
/// - magic: [u8;4] = b"AERO"
/// - format_version: SnapshotVersion
/// - device_id: [u8;4]
/// - device_version: SnapshotVersion
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotHeader {
    pub format_version: SnapshotVersion,
    pub device_id: [u8; 4],
    pub device_version: SnapshotVersion,
}

const MAGIC: [u8; 4] = *b"AERO";
const FORMAT_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);
const HEADER_LEN: usize = 4 + 2 + 2 + 4 + 2 + 2;

pub struct SnapshotWriter {
    device_id: [u8; 4],
    device_version: SnapshotVersion,
    fields: Vec<(u16, Vec<u8>)>,
}

impl SnapshotWriter {
    pub fn new(device_id: [u8; 4], device_version: SnapshotVersion) -> Self {
        Self {
            device_id,
            device_version,
            fields: Vec::new(),
        }
    }

    pub fn field_bytes(&mut self, tag: u16, bytes: Vec<u8>) {
        self.fields.push((tag, bytes));
    }

    pub fn field_u8(&mut self, tag: u16, val: u8) {
        self.field_bytes(tag, vec![val]);
    }

    pub fn field_bool(&mut self, tag: u16, val: bool) {
        self.field_u8(tag, if val { 1 } else { 0 });
    }

    pub fn field_u16(&mut self, tag: u16, val: u16) {
        self.field_bytes(tag, val.to_le_bytes().to_vec());
    }

    pub fn field_u32(&mut self, tag: u16, val: u32) {
        self.field_bytes(tag, val.to_le_bytes().to_vec());
    }

    pub fn field_u64(&mut self, tag: u16, val: u64) {
        self.field_bytes(tag, val.to_le_bytes().to_vec());
    }

    pub fn field_i32(&mut self, tag: u16, val: i32) {
        self.field_bytes(tag, val.to_le_bytes().to_vec());
    }

    pub fn finish(mut self) -> Vec<u8> {
        // Canonical ordering: tags ascending.
        self.fields.sort_by_key(|(tag, _)| *tag);

        let mut out = Vec::with_capacity(HEADER_LEN + self.fields.len() * 8);
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&FORMAT_VERSION.major.to_le_bytes());
        out.extend_from_slice(&FORMAT_VERSION.minor.to_le_bytes());
        out.extend_from_slice(&self.device_id);
        out.extend_from_slice(&self.device_version.major.to_le_bytes());
        out.extend_from_slice(&self.device_version.minor.to_le_bytes());

        for (tag, data) in self.fields {
            out.extend_from_slice(&tag.to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&data);
        }
        out
    }
}

#[derive(Debug)]
pub struct SnapshotReader<'a> {
    header: SnapshotHeader,
    fields: BTreeMap<u16, &'a [u8]>,
}

impl<'a> SnapshotReader<'a> {
    pub fn parse(bytes: &'a [u8], expected_device_id: [u8; 4]) -> SnapshotResult<Self> {
        // The outer TLV is intended for a small number of tagged fields. A corrupted snapshot could
        // encode an extreme number of tiny/empty fields and force pathological BTreeMap growth.
        // Cap the number of fields we will track to keep parsing bounded.
        const MAX_FIELDS: usize = 4096;

        if bytes.len() < HEADER_LEN {
            return Err(SnapshotError::UnexpectedEof);
        }
        if bytes[0..4] != MAGIC {
            return Err(SnapshotError::InvalidMagic);
        }
        let format_version = SnapshotVersion {
            major: u16::from_le_bytes([bytes[4], bytes[5]]),
            minor: u16::from_le_bytes([bytes[6], bytes[7]]),
        };
        if format_version.major != FORMAT_VERSION.major {
            return Err(SnapshotError::UnsupportedFormatVersion {
                found: format_version,
                supported: FORMAT_VERSION,
            });
        }
        let device_id = [bytes[8], bytes[9], bytes[10], bytes[11]];
        if device_id != expected_device_id {
            return Err(SnapshotError::DeviceIdMismatch {
                expected: expected_device_id,
                found: device_id,
            });
        }
        let device_version = SnapshotVersion {
            major: u16::from_le_bytes([bytes[12], bytes[13]]),
            minor: u16::from_le_bytes([bytes[14], bytes[15]]),
        };

        let header = SnapshotHeader {
            format_version,
            device_id,
            device_version,
        };

        let mut fields = BTreeMap::new();
        let mut offset = HEADER_LEN;
        while offset < bytes.len() {
            // Ensure we can read the TLV header (tag + len) without overflowing.
            let header_end = offset.checked_add(6).ok_or(SnapshotError::UnexpectedEof)?;
            if header_end > bytes.len() {
                return Err(SnapshotError::UnexpectedEof);
            }

            let tag = u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
            let len = u32::from_le_bytes([
                bytes[offset + 2],
                bytes[offset + 3],
                bytes[offset + 4],
                bytes[offset + 5],
            ]) as usize;
            offset = header_end;

            let field_end = offset
                .checked_add(len)
                .ok_or(SnapshotError::UnexpectedEof)?;
            if field_end > bytes.len() {
                return Err(SnapshotError::UnexpectedEof);
            }

            if fields.len() >= MAX_FIELDS {
                return Err(SnapshotError::InvalidFieldEncoding("too many fields"));
            }
            if fields.insert(tag, &bytes[offset..field_end]).is_some() {
                return Err(SnapshotError::DuplicateFieldTag(tag));
            }
            offset = field_end;
        }

        Ok(Self { header, fields })
    }

    pub fn header(&self) -> SnapshotHeader {
        self.header
    }

    pub fn ensure_device_major(&self, supported_major: u16) -> SnapshotResult<()> {
        if self.header.device_version.major != supported_major {
            return Err(SnapshotError::UnsupportedDeviceMajorVersion {
                found: self.header.device_version.major,
                supported: supported_major,
            });
        }
        Ok(())
    }

    pub fn bytes(&self, tag: u16) -> Option<&'a [u8]> {
        self.fields.get(&tag).copied()
    }

    /// Iterate over all TLV fields in canonical tag order.
    ///
    /// This is useful for wrapper snapshots that embed a dynamic set of tagged sub-snapshots (e.g.
    /// disk controller snapshots keyed by PCI BDF).
    pub fn iter_fields(&self) -> impl Iterator<Item = (u16, &'a [u8])> + '_ {
        self.fields.iter().map(|(&tag, &bytes)| (tag, bytes))
    }

    pub fn u8(&self, tag: u16) -> SnapshotResult<Option<u8>> {
        let Some(bytes) = self.bytes(tag) else {
            return Ok(None);
        };
        if bytes.len() != 1 {
            return Err(SnapshotError::InvalidFieldEncoding("u8"));
        }
        Ok(Some(bytes[0]))
    }

    pub fn bool(&self, tag: u16) -> SnapshotResult<Option<bool>> {
        let Some(v) = self.u8(tag)? else {
            return Ok(None);
        };
        match v {
            0 => Ok(Some(false)),
            1 => Ok(Some(true)),
            _ => Err(SnapshotError::InvalidFieldEncoding("bool")),
        }
    }

    pub fn u16(&self, tag: u16) -> SnapshotResult<Option<u16>> {
        let Some(bytes) = self.bytes(tag) else {
            return Ok(None);
        };
        if bytes.len() != 2 {
            return Err(SnapshotError::InvalidFieldEncoding("u16"));
        }
        Ok(Some(u16::from_le_bytes([bytes[0], bytes[1]])))
    }

    pub fn u32(&self, tag: u16) -> SnapshotResult<Option<u32>> {
        let Some(bytes) = self.bytes(tag) else {
            return Ok(None);
        };
        if bytes.len() != 4 {
            return Err(SnapshotError::InvalidFieldEncoding("u32"));
        }
        Ok(Some(u32::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3],
        ])))
    }

    pub fn u64(&self, tag: u16) -> SnapshotResult<Option<u64>> {
        let Some(bytes) = self.bytes(tag) else {
            return Ok(None);
        };
        if bytes.len() != 8 {
            return Err(SnapshotError::InvalidFieldEncoding("u64"));
        }
        Ok(Some(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ])))
    }

    pub fn i32(&self, tag: u16) -> SnapshotResult<Option<i32>> {
        let Some(bytes) = self.bytes(tag) else {
            return Ok(None);
        };
        if bytes.len() != 4 {
            return Err(SnapshotError::InvalidFieldEncoding("i32"));
        }
        Ok(Some(i32::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3],
        ])))
    }
}

/// Helpers for deterministic encoding of nested values inside TLV fields.
pub mod codec {
    use super::{SnapshotError, SnapshotResult};

    pub struct Encoder {
        pub buf: Vec<u8>,
    }

    impl Default for Encoder {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Encoder {
        pub fn new() -> Self {
            Self { buf: Vec::new() }
        }

        pub fn bytes(mut self, bytes: &[u8]) -> Self {
            self.buf.extend_from_slice(bytes);
            self
        }

        pub fn u8(mut self, v: u8) -> Self {
            self.buf.push(v);
            self
        }

        pub fn bool(self, v: bool) -> Self {
            self.u8(if v { 1 } else { 0 })
        }

        pub fn u16(mut self, v: u16) -> Self {
            self.buf.extend_from_slice(&v.to_le_bytes());
            self
        }

        pub fn u32(mut self, v: u32) -> Self {
            self.buf.extend_from_slice(&v.to_le_bytes());
            self
        }

        pub fn u64(mut self, v: u64) -> Self {
            self.buf.extend_from_slice(&v.to_le_bytes());
            self
        }

        pub fn i32(mut self, v: i32) -> Self {
            self.buf.extend_from_slice(&v.to_le_bytes());
            self
        }

        pub fn vec_bytes(mut self, values: &[Vec<u8>]) -> Self {
            self = self.u32(values.len() as u32);
            for v in values {
                self = self.u32(v.len() as u32);
                self.buf.extend_from_slice(v);
            }
            self
        }

        pub fn vec_u8(mut self, values: &[u8]) -> Self {
            self = self.u32(values.len() as u32);
            self.buf.extend_from_slice(values);
            self
        }

        pub fn finish(self) -> Vec<u8> {
            self.buf
        }
    }

    pub struct Decoder<'a> {
        buf: &'a [u8],
        offset: usize,
    }

    impl<'a> Decoder<'a> {
        pub fn new(buf: &'a [u8]) -> Self {
            Self { buf, offset: 0 }
        }

        fn take(&mut self, len: usize) -> SnapshotResult<&'a [u8]> {
            let end = self
                .offset
                .checked_add(len)
                .ok_or(SnapshotError::UnexpectedEof)?;
            if end > self.buf.len() {
                return Err(SnapshotError::UnexpectedEof);
            }
            let out = &self.buf[self.offset..end];
            self.offset = end;
            Ok(out)
        }

        pub fn u8(&mut self) -> SnapshotResult<u8> {
            Ok(self.take(1)?[0])
        }

        pub fn bool(&mut self) -> SnapshotResult<bool> {
            match self.u8()? {
                0 => Ok(false),
                1 => Ok(true),
                _ => Err(SnapshotError::InvalidFieldEncoding("bool")),
            }
        }

        pub fn u16(&mut self) -> SnapshotResult<u16> {
            let b = self.take(2)?;
            Ok(u16::from_le_bytes([b[0], b[1]]))
        }

        pub fn u32(&mut self) -> SnapshotResult<u32> {
            let b = self.take(4)?;
            Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        }

        pub fn u64(&mut self) -> SnapshotResult<u64> {
            let b = self.take(8)?;
            Ok(u64::from_le_bytes([
                b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
            ]))
        }

        pub fn i32(&mut self) -> SnapshotResult<i32> {
            let b = self.take(4)?;
            Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        }

        pub fn bytes(&mut self, len: usize) -> SnapshotResult<&'a [u8]> {
            self.take(len)
        }

        pub fn bytes_vec(&mut self, len: usize) -> SnapshotResult<Vec<u8>> {
            let src = self.take(len)?;
            let mut out = Vec::new();
            out.try_reserve_exact(src.len())
                .map_err(|_| SnapshotError::OutOfMemory)?;
            out.extend_from_slice(src);
            Ok(out)
        }

        pub fn vec_bytes(&mut self) -> SnapshotResult<Vec<Vec<u8>>> {
            let count = self.u32()? as usize;
            // `count` is untrusted (loaded from snapshot bytes). Avoid pre-allocating based on it
            // so corrupted snapshots cannot trigger pathological allocations before we validate
            // the declared element lengths.
            let mut out = Vec::new();
            for _ in 0..count {
                let len = self.u32()? as usize;
                out.try_reserve_exact(1)
                    .map_err(|_| SnapshotError::OutOfMemory)?;
                out.push(self.bytes_vec(len)?);
            }
            Ok(out)
        }

        pub fn vec_u8(&mut self) -> SnapshotResult<Vec<u8>> {
            let count = self.u32()? as usize;
            self.bytes_vec(count)
        }

        pub fn finish(self) -> SnapshotResult<()> {
            if self.offset != self.buf.len() {
                return Err(SnapshotError::InvalidFieldEncoding("trailing bytes"));
            }
            Ok(())
        }
    }
}
