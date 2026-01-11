//! AeroGPU WDDM allocation private driver data (Win7 WDDM 1.1).
//!
//! Source of truth: `drivers/aerogpu/protocol/aerogpu_wddm_alloc.h`.
//!
//! This blob is provided by the UMD at allocation creation time and preserved by
//! dxgkrnl for shared allocations, so it can be observed by a different process
//! when opening the shared resource.

/// Magic for [`AerogpuWddmAllocPriv`] (`"ALLO"` little-endian).
pub const AEROGPU_WDDM_ALLOC_PRIV_MAGIC: u32 = 0x414C_4C4F;
/// Version for [`AerogpuWddmAllocPriv`].
pub const AEROGPU_WDDM_ALLOC_PRIV_VERSION: u32 = 1;

/// Maximum value for UMD-generated `alloc_id` (high bit clear).
pub const AEROGPU_WDDM_ALLOC_ID_UMD_MAX: u32 = 0x7FFF_FFFF;
/// Minimum value for KMD-generated `alloc_id` (high bit set).
pub const AEROGPU_WDDM_ALLOC_ID_KMD_MIN: u32 = 0x8000_0000;

/// `flags` bitfield values for [`AerogpuWddmAllocPriv`].
pub const AEROGPU_WDDM_ALLOC_PRIV_FLAG_NONE: u32 = 0;
pub const AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED: u32 = 1u32 << 0;

/// Per-allocation private driver data blob (stable across x86/x64).
///
/// This struct is packed to match the on-the-wire ABI (no pointers; stable
/// across x86/x64).
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuWddmAllocPriv {
    pub magic: u32,
    pub version: u32,
    pub alloc_id: u32,
    pub flags: u32,
    pub share_token: u64,
    pub size_bytes: u64,
    pub reserved0: u64,
}

impl AerogpuWddmAllocPriv {
    pub const SIZE_BYTES: usize = 40;

    pub fn decode_from_le_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < Self::SIZE_BYTES {
            return None;
        }
        Some(Self {
            magic: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            version: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            alloc_id: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
            flags: u32::from_le_bytes(buf[12..16].try_into().unwrap()),
            share_token: u64::from_le_bytes(buf[16..24].try_into().unwrap()),
            size_bytes: u64::from_le_bytes(buf[24..32].try_into().unwrap()),
            reserved0: u64::from_le_bytes(buf[32..40].try_into().unwrap()),
        })
    }
}
