//! Minimal AML encoder/decoder helpers.
//!
//! This module purposefully implements only the small subset of AML needed for
//! our clean-room DSDT. It's not intended to be a general AML library.

pub const AML_OP_SCOPE: u8 = 0x10;
pub const AML_OP_NAME: u8 = 0x08;
pub const AML_OP_METHOD: u8 = 0x14;
pub const AML_OP_PACKAGE: u8 = 0x12;
pub const AML_OP_BUFFER: u8 = 0x11;
pub const AML_OP_STORE: u8 = 0x70;

pub const AML_EXT_OP_PREFIX: u8 = 0x5B;
pub const AML_EXT_OP_DEVICE: u8 = 0x82;

pub const AML_OP_ZERO: u8 = 0x00;
pub const AML_OP_ONE: u8 = 0x01;

pub const AML_OP_BYTE_PREFIX: u8 = 0x0A;
pub const AML_OP_WORD_PREFIX: u8 = 0x0B;
pub const AML_OP_DWORD_PREFIX: u8 = 0x0C;
pub const AML_OP_QWORD_PREFIX: u8 = 0x0E;
pub const AML_OP_STRING_PREFIX: u8 = 0x0D;

pub const AML_OP_ARG0: u8 = 0x68;

pub const AML_NAME_DUAL_PREFIX: u8 = 0x2E;
pub const AML_NAME_MULTI_PREFIX: u8 = 0x2F;
pub const AML_NAME_ROOT_PREFIX: u8 = 0x5C;
pub const AML_NAME_NULL: u8 = 0x00;

pub fn name_seg(name: &str) -> [u8; 4] {
    let bytes = name.as_bytes();
    assert!(
        bytes.len() <= 4,
        "AML name segment must be <= 4 bytes, got {name:?}"
    );
    let mut out = [b'_' ; 4];
    out[..bytes.len()].copy_from_slice(bytes);
    out
}

pub fn name_string(path: &str) -> Vec<u8> {
    if path.is_empty() {
        return vec![AML_NAME_NULL];
    }

    let mut out = Vec::new();
    let mut rest = path;
    if let Some(stripped) = rest.strip_prefix('\\') {
        out.push(AML_NAME_ROOT_PREFIX);
        rest = stripped;
    }

    // Parent prefix '^' is not needed for our DSDT, but handling it is cheap.
    while let Some(stripped) = rest.strip_prefix('^') {
        out.push(b'^');
        rest = stripped;
    }

    let segs: Vec<&str> = rest.split('.').filter(|s| !s.is_empty()).collect();
    assert!(!segs.is_empty(), "invalid AML name string: {path:?}");

    match segs.len() {
        1 => out.extend_from_slice(&name_seg(segs[0])),
        2 => {
            out.push(AML_NAME_DUAL_PREFIX);
            out.extend_from_slice(&name_seg(segs[0]));
            out.extend_from_slice(&name_seg(segs[1]));
        }
        n => {
            assert!(n <= 255, "too many name segments");
            out.push(AML_NAME_MULTI_PREFIX);
            out.push(n as u8);
            for seg in segs {
                out.extend_from_slice(&name_seg(seg));
            }
        }
    }

    out
}

pub fn encode_pkg_length(len: usize) -> Vec<u8> {
    assert!(len <= 0x0FFF_FFFF, "PkgLength too large: {len}");

    if len <= 0x3F {
        return vec![len as u8];
    }

    if len <= 0x0FFF {
        let b0 = 0x40 | (len as u8 & 0x0F);
        let b1 = (len >> 4) as u8;
        return vec![b0, b1];
    }

    if len <= 0x0FFF_FF {
        let b0 = 0x80 | (len as u8 & 0x0F);
        let b1 = (len >> 4) as u8;
        let b2 = (len >> 12) as u8;
        return vec![b0, b1, b2];
    }

    let b0 = 0xC0 | (len as u8 & 0x0F);
    let b1 = (len >> 4) as u8;
    let b2 = (len >> 12) as u8;
    let b3 = (len >> 20) as u8;
    vec![b0, b1, b2, b3]
}

pub fn parse_pkg_length(bytes: &[u8], offset: usize) -> Option<(usize, usize)> {
    let b0 = *bytes.get(offset)?;
    let follow_bytes = (b0 >> 6) as usize;
    let mut len: usize = (b0 & 0x3F) as usize;
    for i in 0..follow_bytes {
        let b = *bytes.get(offset + 1 + i)?;
        len |= (b as usize) << (4 + i * 8);
    }
    Some((len, 1 + follow_bytes))
}

pub fn op_scope(name: &str, body: Vec<u8>) -> Vec<u8> {
    let mut content = Vec::new();
    content.extend_from_slice(&name_string(name));
    content.extend_from_slice(&body);

    let mut out = Vec::new();
    out.push(AML_OP_SCOPE);
    out.extend_from_slice(&encode_pkg_length(content.len()));
    out.extend_from_slice(&content);
    out
}

pub fn op_device(name: &str, body: Vec<u8>) -> Vec<u8> {
    let mut content = Vec::new();
    content.extend_from_slice(&name_string(name));
    content.extend_from_slice(&body);

    let mut out = Vec::new();
    out.push(AML_EXT_OP_PREFIX);
    out.push(AML_EXT_OP_DEVICE);
    out.extend_from_slice(&encode_pkg_length(content.len()));
    out.extend_from_slice(&content);
    out
}

pub fn op_name(name: &str, value: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(AML_OP_NAME);
    out.extend_from_slice(&name_string(name));
    out.extend_from_slice(&value);
    out
}

pub fn op_method(name: &str, arg_count: u8, serialized: bool, body: Vec<u8>) -> Vec<u8> {
    assert!(arg_count <= 7);
    let flags = (arg_count & 0x07) | if serialized { 0x08 } else { 0x00 };

    let mut content = Vec::new();
    content.extend_from_slice(&name_string(name));
    content.push(flags);
    content.extend_from_slice(&body);

    let mut out = Vec::new();
    out.push(AML_OP_METHOD);
    out.extend_from_slice(&encode_pkg_length(content.len()));
    out.extend_from_slice(&content);
    out
}

pub fn op_store(src: Vec<u8>, dst_name: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(AML_OP_STORE);
    out.extend_from_slice(&src);
    out.extend_from_slice(&name_string(dst_name));
    out
}

pub fn op_package(elements: Vec<Vec<u8>>) -> Vec<u8> {
    assert!(elements.len() <= 255);
    let mut content = Vec::new();
    content.push(elements.len() as u8);
    for el in elements {
        content.extend_from_slice(&el);
    }

    let mut out = Vec::new();
    out.push(AML_OP_PACKAGE);
    out.extend_from_slice(&encode_pkg_length(content.len()));
    out.extend_from_slice(&content);
    out
}

pub fn op_buffer(raw: &[u8]) -> Vec<u8> {
    let mut content = Vec::new();
    content.extend_from_slice(&op_integer(raw.len() as u64));
    content.extend_from_slice(raw);

    let mut out = Vec::new();
    out.push(AML_OP_BUFFER);
    out.extend_from_slice(&encode_pkg_length(content.len()));
    out.extend_from_slice(&content);
    out
}

pub fn op_integer(value: u64) -> Vec<u8> {
    match value {
        0 => vec![AML_OP_ZERO],
        1 => vec![AML_OP_ONE],
        0..=0xFF => vec![AML_OP_BYTE_PREFIX, value as u8],
        0..=0xFFFF => {
            let mut out = vec![AML_OP_WORD_PREFIX];
            out.extend_from_slice(&(value as u16).to_le_bytes());
            out
        }
        0..=0xFFFF_FFFF => {
            let mut out = vec![AML_OP_DWORD_PREFIX];
            out.extend_from_slice(&(value as u32).to_le_bytes());
            out
        }
        _ => {
            let mut out = vec![AML_OP_QWORD_PREFIX];
            out.extend_from_slice(&value.to_le_bytes());
            out
        }
    }
}

pub fn op_string(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(AML_OP_STRING_PREFIX);
    out.extend_from_slice(s.as_bytes());
    out.push(0);
    out
}
