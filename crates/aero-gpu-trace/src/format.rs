use std::fmt;

pub const TRACE_MAGIC: [u8; 8] = *b"AEROGPUT";
pub const TOC_MAGIC: [u8; 8] = *b"AEROTOC\0";
pub const FOOTER_MAGIC: [u8; 8] = *b"AEROGPUF";

pub const TRACE_HEADER_SIZE: u32 = 32;
pub const TRACE_FOOTER_SIZE: u32 = 32;
pub const TRACE_RECORD_HEADER_SIZE: u32 = 8;
pub const TRACE_BLOB_HEADER_SIZE: u32 = 16;
pub const TOC_HEADER_SIZE: u32 = 16;
pub const TOC_ENTRY_SIZE: u32 = 32;

pub const CONTAINER_VERSION: u32 = 1;
pub const TOC_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum RecordType {
    BeginFrame = 0x01,
    Present = 0x02,
    Packet = 0x03,
    Blob = 0x04,
}

impl RecordType {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0x01 => Some(Self::BeginFrame),
            0x02 => Some(Self::Present),
            0x03 => Some(Self::Packet),
            0x04 => Some(Self::Blob),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum BlobKind {
    BufferData = 0x01,
    TextureData = 0x02,
    ShaderDxbc = 0x03,
    ShaderWgsl = 0x04,
    ShaderGlslEs300 = 0x05,
}

impl BlobKind {
    pub fn from_u32(value: u32) -> Option<Self> {
        match value {
            0x01 => Some(Self::BufferData),
            0x02 => Some(Self::TextureData),
            0x03 => Some(Self::ShaderDxbc),
            0x04 => Some(Self::ShaderWgsl),
            0x05 => Some(Self::ShaderGlslEs300),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceMeta {
    pub emulator_version: String,
    pub command_abi_version: u32,
    pub notes: Option<String>,
}

impl TraceMeta {
    pub fn new(emulator_version: impl Into<String>, command_abi_version: u32) -> Self {
        Self {
            emulator_version: emulator_version.into(),
            command_abi_version,
            notes: None,
        }
    }

    pub fn to_json_bytes(&self) -> Vec<u8> {
        fn escape_json_string(s: &str, out: &mut String) {
            for c in s.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if c.is_control() => {
                        // JSON escape
                        use std::fmt::Write;
                        let _ = write!(out, "\\u{:04x}", c as u32);
                    }
                    _ => out.push(c),
                }
            }
        }

        let mut json = String::new();
        json.push('{');
        json.push_str("\"emulator_version\":\"");
        escape_json_string(&self.emulator_version, &mut json);
        json.push_str("\",\"command_abi_version\":");
        json.push_str(&self.command_abi_version.to_string());
        if let Some(notes) = &self.notes {
            json.push_str(",\"notes\":\"");
            escape_json_string(notes, &mut json);
            json.push('"');
        }
        json.push('}');
        json.into_bytes()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct TraceHeader {
    pub container_version: u32,
    pub command_abi_version: u32,
    pub flags: u32,
    pub meta_len: u32,
}

impl fmt::Debug for TraceHeader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TraceHeader")
            .field("container_version", &self.container_version)
            .field("command_abi_version", &self.command_abi_version)
            .field("flags", &format_args!("0x{:08x}", self.flags))
            .field("meta_len", &self.meta_len)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceFooter {
    pub container_version: u32,
    pub toc_offset: u64,
    pub toc_len: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameTocEntry {
    pub frame_index: u32,
    pub flags: u32,
    pub start_offset: u64,
    pub present_offset: u64,
    pub end_offset: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceToc {
    pub entries: Vec<FrameTocEntry>,
}

impl TraceToc {
    pub fn len_bytes(&self) -> u64 {
        TOC_HEADER_SIZE as u64 + (self.entries.len() as u64) * (TOC_ENTRY_SIZE as u64)
    }
}
