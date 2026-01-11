//! AeroGPU allocation private driver data (KMD → UMD).
//!
//! Source of truth: `drivers/aerogpu/protocol/aerogpu_alloc_privdata.h`.

/// Magic for [`AerogpuAllocPrivdata`] (`"ALPD"` little-endian).
pub const AEROGPU_ALLOC_PRIVDATA_MAGIC: u32 = 0x4450_4C41;
/// Version for [`AerogpuAllocPrivdata`].
pub const AEROGPU_ALLOC_PRIVDATA_VERSION: u32 = 1;

/// Per-allocation private data blob produced by the AeroGPU KMD for shareable allocations.
///
/// This struct is packed to match the on-the-wire ABI (no pointers; stable across x86/x64).
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuAllocPrivdata {
    pub magic: u32,
    pub version: u32,
    pub share_token: u64,
    pub reserved0: u64,
}

impl AerogpuAllocPrivdata {
    pub const SIZE_BYTES: usize = 24;

    pub fn decode_from_le_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < Self::SIZE_BYTES {
            return None;
        }
        Some(Self {
            magic: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            version: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            share_token: u64::from_le_bytes(buf[8..16].try_into().unwrap()),
            reserved0: u64::from_le_bytes(buf[16..24].try_into().unwrap()),
        })
    }
}
