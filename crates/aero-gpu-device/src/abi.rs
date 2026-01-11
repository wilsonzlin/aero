//! Prototype guest↔host GPU command ABI (AGRN/AGPC).
//!
//! The ABI is intentionally simple:
//! - Commands are written by the guest into a byte ring (`GpuRingHeader` + data).
//! - The host reads commands, executes them against a backend, and writes a
//!   `GpuCompletion` record for each processed command into a completion ring.
//! - A doorbell MMIO write triggers processing; completions raise an interrupt.
//!
//! This module is the "wire format". Avoid putting host-only conveniences here.

#![cfg_attr(not(target_endian = "little"), allow(dead_code))]

#[cfg(not(target_endian = "little"))]
compile_error!("aero-gpu-device ABI is defined as little-endian only.");

/// ABI major version.
pub const ABI_MAJOR: u16 = 1;
/// ABI minor version.
pub const ABI_MINOR: u16 = 0;

pub const fn fourcc(tag: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*tag)
}

pub const GPU_RING_MAGIC: u32 = fourcc(b"AGRN");
pub const GPU_CMD_MAGIC: u32 = fourcc(b"AGPC");
pub const GPU_CPL_MAGIC: u32 = fourcc(b"AGCP");
pub const GPU_PAD_MAGIC: u32 = fourcc(b"AGPD");

/// Common record header for any ring payload.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct GpuRecordHeader {
    pub magic: u32,
    pub size_bytes: u32,
}

impl GpuRecordHeader {
    pub const SIZE: usize = 8;
}

/// Ring header stored in guest physical memory.
///
/// Head/tail are byte offsets within the data region `[0, ring_size_bytes)`.
/// Both producer and consumer must update the offsets with atomic semantics
/// (release on writes, acquire on reads) when used concurrently.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct GpuRingHeader {
    pub magic: u32,
    pub abi_major: u16,
    pub abi_minor: u16,
    pub ring_size_bytes: u32,
    pub head: u32,
    pub tail: u32,
    pub _reserved: [u32; 11],
}

impl GpuRingHeader {
    pub const SIZE: usize = 64;
}

/// Header of every command in the command ring.
///
/// The host must be able to skip unknown opcodes by consulting `size_bytes`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct GpuCmdHeader {
    pub record: GpuRecordHeader,
    pub opcode: u16,
    pub flags: u16,
    pub abi_major: u16,
    pub abi_minor: u16,
    pub seq: u64,
}

impl GpuCmdHeader {
    pub const SIZE: usize = 24;
}

/// Completion entry written by the host to the completion ring.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct GpuCompletion {
    pub record: GpuRecordHeader,
    pub seq: u64,
    pub opcode: u16,
    pub _reserved0: u16,
    pub status: u32,
}

impl GpuCompletion {
    pub const SIZE: usize = 24;
}

/// Status codes for `GpuCompletion.status`.
pub mod status {
    pub const OK: u32 = 0;
    pub const INVALID_COMMAND: u32 = 1;
    pub const INVALID_RESOURCE: u32 = 2;
    pub const OUT_OF_BOUNDS: u32 = 3;
    pub const UNSUPPORTED: u32 = 4;
    pub const INTERNAL_ERROR: u32 = 0xFFFF_FFFF;
}

/// Command opcodes.
pub mod opcode {
    pub const NOP: u16 = 0x0000;

    pub const CREATE_BUFFER: u16 = 0x0001;
    pub const DESTROY_BUFFER: u16 = 0x0002;
    pub const WRITE_BUFFER: u16 = 0x0003;
    pub const READ_BUFFER: u16 = 0x0004;

    pub const CREATE_TEXTURE2D: u16 = 0x0010;
    pub const DESTROY_TEXTURE: u16 = 0x0011;
    pub const WRITE_TEXTURE2D: u16 = 0x0012;
    pub const READ_TEXTURE2D: u16 = 0x0013;

    pub const SET_RENDER_TARGET: u16 = 0x0020;
    pub const CLEAR: u16 = 0x0021;
    pub const SET_VIEWPORT: u16 = 0x0022;

    pub const SET_PIPELINE: u16 = 0x0030;
    pub const SET_VERTEX_BUFFER: u16 = 0x0031;
    pub const DRAW: u16 = 0x0032;

    pub const PRESENT: u16 = 0x0040;

    pub const FENCE_SIGNAL: u16 = 0x0050;
}

/// Buffer usage bitmask (little-endian `u32` on the wire).
pub mod buffer_usage {
    pub const TRANSFER_SRC: u32 = 1 << 0;
    pub const TRANSFER_DST: u32 = 1 << 1;
    pub const VERTEX: u32 = 1 << 2;
    pub const INDEX: u32 = 1 << 3;
    pub const UNIFORM: u32 = 1 << 4;
}

/// Texture usage bitmask (little-endian `u32` on the wire).
pub mod texture_usage {
    pub const TRANSFER_SRC: u32 = 1 << 0;
    pub const TRANSFER_DST: u32 = 1 << 1;
    pub const RENDER_ATTACHMENT: u32 = 1 << 2;
    pub const TEXTURE_BINDING: u32 = 1 << 3;
}

/// Texture formats (little-endian `u32` on the wire).
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextureFormat {
    Rgba8Unorm = 1,
}

impl TextureFormat {
    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            1 => Some(Self::Rgba8Unorm),
            _ => None,
        }
    }
}

/// Built-in pipeline IDs (until shader modules are supported by the ABI).
pub mod pipeline {
    /// Vertex format: `pos: f32x2, color: f32x4`, topology: triangle list.
    pub const BASIC_VERTEX_COLOR: u32 = 1;
}

/// MMIO register map for the virtual PCI GPU.
pub mod mmio {
    /// Read-only: `(ABI_MAJOR << 16) | ABI_MINOR`.
    pub const REG_ABI_VERSION: u64 = 0x000;

    pub const REG_CMD_RING_BASE_LO: u64 = 0x100;
    pub const REG_CMD_RING_BASE_HI: u64 = 0x104;
    pub const REG_CMD_RING_SIZE: u64 = 0x108;

    pub const REG_CPL_RING_BASE_LO: u64 = 0x110;
    pub const REG_CPL_RING_BASE_HI: u64 = 0x114;
    pub const REG_CPL_RING_SIZE: u64 = 0x118;

    pub const REG_DESC_BASE_LO: u64 = 0x120;
    pub const REG_DESC_BASE_HI: u64 = 0x124;
    pub const REG_DESC_SIZE: u64 = 0x128;

    /// Writing any value rings the doorbell (process more commands).
    pub const REG_DOORBELL: u64 = 0x200;

    pub const REG_INT_STATUS: u64 = 0x300;
    pub const REG_INT_MASK: u64 = 0x304;
    pub const REG_INT_ACK: u64 = 0x308;

    pub const INT_STATUS_CPL_AVAIL: u32 = 1 << 0;
    pub const INT_STATUS_FAULT: u32 = 1 << 1;

    /// Read-only: last completed command sequence number (low/high).
    pub const REG_LAST_COMPLETED_SEQ_LO: u64 = 0x310;
    pub const REG_LAST_COMPLETED_SEQ_HI: u64 = 0x314;

    /// Read-only: last faulting command sequence number (low/high).
    pub const REG_LAST_FAULT_SEQ_LO: u64 = 0x318;
    pub const REG_LAST_FAULT_SEQ_HI: u64 = 0x31C;
}

/// PCI enumeration info for the virtual GPU.
pub mod pci {
    /// Aero (unregistered) vendor ID.
    pub const VENDOR_ID: u16 = 0xA0E0;
    /// Aero virtual GPU device ID.
    pub const DEVICE_ID: u16 = 0x0001;

    /// PCI class: Display controller (0x03).
    pub const CLASS_CODE: u8 = 0x03;
    /// Subclass: 3D controller (0x02).
    pub const SUBCLASS: u8 = 0x02;
    pub const PROG_IF: u8 = 0x00;

    /// BAR0: MMIO registers (4 KiB).
    pub const BAR0_SIZE: u32 = 0x1000;
}

/// Command payload structs.
///
/// These are primarily for documentation; the command processor parses them from
/// bytes and tolerates larger `size_bytes` to allow future extension.
pub mod cmd {
    use super::GpuCmdHeader;

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct CreateBuffer {
        pub header: GpuCmdHeader,
        pub buffer_id: u32,
        pub _reserved0: u32,
        pub size_bytes: u64,
        pub usage: u32,
        pub _reserved1: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct DestroyBuffer {
        pub header: GpuCmdHeader,
        pub buffer_id: u32,
        pub _reserved0: u32,
        pub _reserved1: u64,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct WriteBuffer {
        pub header: GpuCmdHeader,
        pub buffer_id: u32,
        pub _reserved0: u32,
        pub dst_offset: u64,
        pub src_paddr: u64,
        pub size_bytes: u32,
        pub _reserved1: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct ReadBuffer {
        pub header: GpuCmdHeader,
        pub buffer_id: u32,
        pub _reserved0: u32,
        pub src_offset: u64,
        pub dst_paddr: u64,
        pub size_bytes: u32,
        pub _reserved1: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct CreateTexture2d {
        pub header: GpuCmdHeader,
        pub texture_id: u32,
        pub width: u32,
        pub height: u32,
        pub format: u32,
        pub usage: u32,
        pub _reserved0: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct DestroyTexture {
        pub header: GpuCmdHeader,
        pub texture_id: u32,
        pub _reserved0: u32,
        pub _reserved1: u64,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct WriteTexture2d {
        pub header: GpuCmdHeader,
        pub texture_id: u32,
        pub mip_level: u32,
        pub src_paddr: u64,
        pub bytes_per_row: u32,
        pub width: u32,
        pub height: u32,
        pub _reserved0: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct ReadTexture2d {
        pub header: GpuCmdHeader,
        pub texture_id: u32,
        pub mip_level: u32,
        pub dst_paddr: u64,
        pub bytes_per_row: u32,
        pub width: u32,
        pub height: u32,
        pub _reserved0: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct SetRenderTarget {
        pub header: GpuCmdHeader,
        pub texture_id: u32,
        pub _reserved0: u32,
        pub _reserved1: u64,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct Clear {
        pub header: GpuCmdHeader,
        pub rgba: [f32; 4],
        pub _reserved0: u64,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct SetViewport {
        pub header: GpuCmdHeader,
        pub x: f32,
        pub y: f32,
        pub width: f32,
        pub height: f32,
        pub _reserved0: u64,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct SetPipeline {
        pub header: GpuCmdHeader,
        pub pipeline_id: u32,
        pub _reserved0: u32,
        pub _reserved1: u64,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct SetVertexBuffer {
        pub header: GpuCmdHeader,
        pub buffer_id: u32,
        pub stride: u32,
        pub offset: u64,
        pub _reserved0: u64,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct Draw {
        pub header: GpuCmdHeader,
        pub vertex_count: u32,
        pub first_vertex: u32,
        pub _reserved0: u64,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct Present {
        pub header: GpuCmdHeader,
        pub texture_id: u32,
        pub _reserved0: u32,
        pub _reserved1: u64,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct FenceSignal {
        pub header: GpuCmdHeader,
        pub fence_id: u32,
        pub _reserved0: u32,
        pub value: u64,
        pub _reserved1: u64,
    }
}
