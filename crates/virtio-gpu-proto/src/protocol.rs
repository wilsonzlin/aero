//! Minimal virtio-gpu wire format definitions.
//!
//! Reference: virtio-gpu specification as implemented by QEMU and Linux (`include/uapi/linux/virtio_gpu.h`).
//! We only model the subset required for basic 2D scanout.

use core::fmt;

// --- Virtio IDs (virtio 1.0 transitional PCI device IDs) --------------------
//
// These are *constants only* here; this crate does not implement virtio-pci.
//
// Vendor: Red Hat, Inc. (standard virtio vendor for PCI).
pub const VIRTIO_PCI_VENDOR_ID: u16 = 0x1af4;
// Device ID for virtio-gpu-pci (transitional).
pub const VIRTIO_PCI_DEVICE_ID_GPU: u16 = 0x1050;

// --- Virtio-gpu control types -----------------------------------------------

// Commands (0x01xx)
pub const VIRTIO_GPU_CMD_GET_DISPLAY_INFO: u32 = 0x0100;
pub const VIRTIO_GPU_CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
pub const VIRTIO_GPU_CMD_RESOURCE_UNREF: u32 = 0x0102;
pub const VIRTIO_GPU_CMD_SET_SCANOUT: u32 = 0x0103;
pub const VIRTIO_GPU_CMD_RESOURCE_FLUSH: u32 = 0x0104;
pub const VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D: u32 = 0x0105;
pub const VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;
pub const VIRTIO_GPU_CMD_RESOURCE_DETACH_BACKING: u32 = 0x0107;

// Responses (0x11xx)
pub const VIRTIO_GPU_RESP_OK_NODATA: u32 = 0x1100;
pub const VIRTIO_GPU_RESP_OK_DISPLAY_INFO: u32 = 0x1101;
pub const VIRTIO_GPU_RESP_ERR_UNSPEC: u32 = 0x1200;
pub const VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER: u32 = 0x1203;

// Formats (subset)
// Most Windows virtual display stacks prefer BGRA.
pub const VIRTIO_GPU_FORMAT_B8G8R8A8_UNORM: u32 = 1;

pub const VIRTIO_GPU_MAX_SCANOUTS: usize = 16;

// --- Helpers ----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CtrlHdr {
    pub type_: u32,
    pub flags: u32,
    pub fence_id: u64,
    pub ctx_id: u32,
}

impl CtrlHdr {
    pub const WIREFORMAT_SIZE: usize = 24;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolError {
    BufferTooShort { want: usize, got: usize },
    UnknownCommand(u32),
    InvalidParameter(&'static str),
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProtocolError::BufferTooShort { want, got } => {
                write!(f, "buffer too short (want {want}, got {got})")
            }
            ProtocolError::UnknownCommand(ty) => write!(f, "unknown virtio-gpu command type 0x{ty:08x}"),
            ProtocolError::InvalidParameter(msg) => write!(f, "invalid parameter: {msg}"),
        }
    }
}

impl std::error::Error for ProtocolError {}

#[inline]
pub fn read_u32_le(buf: &[u8], off: usize) -> Result<u32, ProtocolError> {
    let end = off.checked_add(4).ok_or(ProtocolError::InvalidParameter("overflow"))?;
    let bytes = buf.get(off..end).ok_or(ProtocolError::BufferTooShort {
        want: end,
        got: buf.len(),
    })?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}

#[inline]
pub fn read_u64_le(buf: &[u8], off: usize) -> Result<u64, ProtocolError> {
    let end = off.checked_add(8).ok_or(ProtocolError::InvalidParameter("overflow"))?;
    let bytes = buf.get(off..end).ok_or(ProtocolError::BufferTooShort {
        want: end,
        got: buf.len(),
    })?;
    Ok(u64::from_le_bytes(bytes.try_into().unwrap()))
}

#[inline]
pub fn write_u32_le(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

#[inline]
pub fn write_u64_le(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

pub fn parse_ctrl_hdr(req: &[u8]) -> Result<CtrlHdr, ProtocolError> {
    if req.len() < CtrlHdr::WIREFORMAT_SIZE {
        return Err(ProtocolError::BufferTooShort {
            want: CtrlHdr::WIREFORMAT_SIZE,
            got: req.len(),
        });
    }
    Ok(CtrlHdr {
        type_: read_u32_le(req, 0)?,
        flags: read_u32_le(req, 4)?,
        fence_id: read_u64_le(req, 8)?,
        ctx_id: read_u32_le(req, 16)?,
    })
}

pub fn parse_rect(req: &[u8], off: usize) -> Result<Rect, ProtocolError> {
    Ok(Rect {
        x: read_u32_le(req, off)?,
        y: read_u32_le(req, off + 4)?,
        width: read_u32_le(req, off + 8)?,
        height: read_u32_le(req, off + 12)?,
    })
}

pub fn encode_resp_hdr_from_req(req: &CtrlHdr, resp_type: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(CtrlHdr::WIREFORMAT_SIZE);
    write_u32_le(&mut out, resp_type);
    write_u32_le(&mut out, req.flags);
    write_u64_le(&mut out, req.fence_id);
    write_u32_le(&mut out, req.ctx_id);
    write_u32_le(&mut out, 0); // padding
    out
}

