//! AeroGPU UMD-private discovery blob (UMDRIVERPRIVATE).
//!
//! Source of truth: `drivers/aerogpu/protocol/aerogpu_umd_private.h`.

/// ABI struct version for [`AerogpuUmdPrivateV1`].
pub const AEROGPU_UMDPRIV_STRUCT_VERSION_V1: u32 = 1;

/// Raw BAR0[0] magic for the legacy AeroGPU MMIO ABI ("ARGP").
pub const AEROGPU_UMDPRIV_MMIO_MAGIC_LEGACY_ARGP: u32 = 0x4152_4750; // "ARGP" LE
/// Raw BAR0[0] magic for the new AeroGPU MMIO ABI ("AGPU").
pub const AEROGPU_UMDPRIV_MMIO_MAGIC_NEW_AGPU: u32 = 0x5550_4741; // "AGPU" LE

/// Offsets shared by both legacy/new ABIs for device discovery.
pub const AEROGPU_UMDPRIV_MMIO_REG_MAGIC: u32 = 0x0000;
pub const AEROGPU_UMDPRIV_MMIO_REG_ABI_VERSION: u32 = 0x0004;
pub const AEROGPU_UMDPRIV_MMIO_REG_FEATURES_LO: u32 = 0x0008;
pub const AEROGPU_UMDPRIV_MMIO_REG_FEATURES_HI: u32 = 0x000C;

/// Feature bits (mirrors `aerogpu_pci.h` for the new "AGPU" ABI).
pub const AEROGPU_UMDPRIV_FEATURE_FENCE_PAGE: u64 = 1u64 << 0;
pub const AEROGPU_UMDPRIV_FEATURE_CURSOR: u64 = 1u64 << 1;
pub const AEROGPU_UMDPRIV_FEATURE_SCANOUT: u64 = 1u64 << 2;
pub const AEROGPU_UMDPRIV_FEATURE_VBLANK: u64 = 1u64 << 3;

/// `flags` bitfield values for [`AerogpuUmdPrivateV1`].
pub const AEROGPU_UMDPRIV_FLAG_IS_LEGACY: u32 = 1u32 << 0;
pub const AEROGPU_UMDPRIV_FLAG_HAS_VBLANK: u32 = 1u32 << 1;
pub const AEROGPU_UMDPRIV_FLAG_HAS_FENCE_PAGE: u32 = 1u32 << 2;

/// Version 1 of the UMDRIVERPRIVATE discovery blob returned by the KMD.
///
/// This struct is packed to match the on-the-wire ABI (no pointers; stable across x86/x64).
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AerogpuUmdPrivateV1 {
    pub size_bytes: u32,
    pub struct_version: u32,
    pub device_mmio_magic: u32,
    pub device_abi_version_u32: u32,
    pub reserved0: u32,
    pub device_features: u64,
    pub flags: u32,
    pub reserved1: u32,
    pub reserved2: u32,
    pub reserved3: [u64; 3],
}

impl AerogpuUmdPrivateV1 {
    pub const SIZE_BYTES: usize = 64;

    pub fn decode_from_le_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < Self::SIZE_BYTES {
            return None;
        }
        Some(Self {
            size_bytes: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            struct_version: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            device_mmio_magic: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
            device_abi_version_u32: u32::from_le_bytes(buf[12..16].try_into().unwrap()),
            reserved0: u32::from_le_bytes(buf[16..20].try_into().unwrap()),
            device_features: u64::from_le_bytes(buf[20..28].try_into().unwrap()),
            flags: u32::from_le_bytes(buf[28..32].try_into().unwrap()),
            reserved1: u32::from_le_bytes(buf[32..36].try_into().unwrap()),
            reserved2: u32::from_le_bytes(buf[36..40].try_into().unwrap()),
            reserved3: [
                u64::from_le_bytes(buf[40..48].try_into().unwrap()),
                u64::from_le_bytes(buf[48..56].try_into().unwrap()),
                u64::from_le_bytes(buf[56..64].try_into().unwrap()),
            ],
        })
    }
}
