//! AeroGPU WDDM allocation private-driver-data contract (Win7 WDDM 1.1).
//!
//! Source of truth: `drivers/aerogpu/protocol/aerogpu_wddm_alloc.h`.
//!
//! The UMD provides a per-allocation private-data buffer at creation time, and the KMD fills it
//! during `DxgkDdiCreateAllocation` / `DxgkDdiOpenAllocation`. For shared allocations, dxgkrnl
//! preserves and replays the blob across processes so the opening UMD instance observes the same
//! `alloc_id` / `share_token`.

/// Magic for [`AerogpuWddmAllocPriv`] (`"ALLO"` little-endian).
pub const AEROGPU_WDDM_ALLOC_PRIV_MAGIC: u32 = 0x414C_4C4F;
/// Version for [`AerogpuWddmAllocPriv`].
pub const AEROGPU_WDDM_ALLOC_PRIV_VERSION: u32 = 1;
/// Version for [`AerogpuWddmAllocPrivV2`].
pub const AEROGPU_WDDM_ALLOC_PRIV_VERSION_2: u32 = 2;

// Backwards-compat aliases (older code used *_PRIVATE_DATA_* names).
pub const AEROGPU_WDDM_ALLOC_PRIVATE_DATA_MAGIC: u32 = AEROGPU_WDDM_ALLOC_PRIV_MAGIC;
pub const AEROGPU_WDDM_ALLOC_PRIVATE_DATA_VERSION: u32 = AEROGPU_WDDM_ALLOC_PRIV_VERSION;

/// Maximum value for UMD-generated `alloc_id` (high bit clear).
pub const AEROGPU_WDDM_ALLOC_ID_UMD_MAX: u32 = 0x7FFF_FFFF;
/// Minimum value for KMD-generated `alloc_id` (high bit set).
pub const AEROGPU_WDDM_ALLOC_ID_KMD_MIN: u32 = 0x8000_0000;

/// `flags` bitfield values for [`AerogpuWddmAllocPriv`].
pub const AEROGPU_WDDM_ALLOC_PRIV_FLAG_NONE: u32 = 0;
pub const AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED: u32 = 1u32 << 0;
pub const AEROGPU_WDDM_ALLOC_PRIV_FLAG_CPU_VISIBLE: u32 = 1u32 << 1;
pub const AEROGPU_WDDM_ALLOC_PRIV_FLAG_STAGING: u32 = 1u32 << 2;

/// Backwards-compat alias for [`AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED`].
pub const AEROGPU_WDDM_ALLOC_PRIV_FLAG_SHARED: u32 = AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED;

/// Marker bit for `reserved0` description encoding (bit 63 set).
pub const AEROGPU_WDDM_ALLOC_PRIV_DESC_MARKER: u64 = 0x8000_0000_0000_0000;
pub const AEROGPU_WDDM_ALLOC_PRIV_DESC_MAX_WIDTH: u32 = 0xFFFF;
pub const AEROGPU_WDDM_ALLOC_PRIV_DESC_MAX_HEIGHT: u32 = 0x7FFF;

pub const fn aerogpu_wddm_alloc_priv_desc_pack(
    format_u32: u32,
    width_u32: u32,
    height_u32: u32,
) -> u64 {
    AEROGPU_WDDM_ALLOC_PRIV_DESC_MARKER
        | (format_u32 as u64 & 0xFFFF_FFFF)
        | ((width_u32 as u64 & 0xFFFF) << 32)
        | ((height_u32 as u64 & 0x7FFF) << 48)
}

pub const fn aerogpu_wddm_alloc_priv_desc_present(desc_u64: u64) -> bool {
    (desc_u64 & AEROGPU_WDDM_ALLOC_PRIV_DESC_MARKER) != 0
}

pub const fn aerogpu_wddm_alloc_priv_desc_format(desc_u64: u64) -> u32 {
    (desc_u64 & 0xFFFF_FFFF) as u32
}

pub const fn aerogpu_wddm_alloc_priv_desc_width(desc_u64: u64) -> u32 {
    ((desc_u64 >> 32) & 0xFFFF) as u32
}

pub const fn aerogpu_wddm_alloc_priv_desc_height(desc_u64: u64) -> u32 {
    ((desc_u64 >> 48) & 0x7FFF) as u32
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AerogpuWddmAllocKind {
    Unknown = 0,
    Buffer = 1,
    Texture2d = 2,
}

/// Per-allocation WDDM "private driver data" blob (UMD ↔ KMD via dxgkrnl).
///
/// This struct is packed to match the on-the-wire ABI (no pointers; stable across x86/x64).
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

/// WDDM allocation private-driver-data struct version 2 (UMD → dxgkrnl → KMD).
///
/// v2 extends [`AerogpuWddmAllocPriv`] with explicit resource metadata so OpenResource paths can
/// recover texture properties (width/height/format/row_pitch) without depending on DDI-specific
/// per-resource structs.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuWddmAllocPrivV2 {
    pub magic: u32,
    pub version: u32,
    pub alloc_id: u32,
    pub flags: u32,
    pub share_token: u64,
    pub size_bytes: u64,
    pub reserved0: u64,
    pub kind: u32,
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub row_pitch_bytes: u32,
    pub reserved1: u32,
}

impl AerogpuWddmAllocPrivV2 {
    pub const SIZE_BYTES: usize = 64;

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
            kind: u32::from_le_bytes(buf[40..44].try_into().unwrap()),
            width: u32::from_le_bytes(buf[44..48].try_into().unwrap()),
            height: u32::from_le_bytes(buf[48..52].try_into().unwrap()),
            format: u32::from_le_bytes(buf[52..56].try_into().unwrap()),
            row_pitch_bytes: u32::from_le_bytes(buf[56..60].try_into().unwrap()),
            reserved1: u32::from_le_bytes(buf[60..64].try_into().unwrap()),
        })
    }
}

#[derive(Clone, Copy)]
pub enum AerogpuWddmAllocPrivAny {
    V1(AerogpuWddmAllocPriv),
    V2(AerogpuWddmAllocPrivV2),
}

impl AerogpuWddmAllocPrivAny {
    pub fn decode_from_le_bytes(buf: &[u8]) -> Option<Self> {
        let base = AerogpuWddmAllocPriv::decode_from_le_bytes(buf)?;
        if base.magic != AEROGPU_WDDM_ALLOC_PRIV_MAGIC {
            return None;
        }

        match base.version {
            AEROGPU_WDDM_ALLOC_PRIV_VERSION => Some(Self::V1(base)),
            AEROGPU_WDDM_ALLOC_PRIV_VERSION_2 => {
                AerogpuWddmAllocPrivV2::decode_from_le_bytes(buf).map(Self::V2)
            }
            _ => None,
        }
    }
}
