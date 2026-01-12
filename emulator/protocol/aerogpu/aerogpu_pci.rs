//! AeroGPU PCI/MMIO constants and ABI version helpers.
//!
//! Source of truth: `drivers/aerogpu/protocol/aerogpu_pci.h`.

/// ABI major version (breaking changes).
pub const AEROGPU_ABI_MAJOR: u32 = 1;
/// ABI minor version (backwards-compatible extensions).
pub const AEROGPU_ABI_MINOR: u32 = 2;

pub const AEROGPU_ABI_VERSION_U32: u32 = (AEROGPU_ABI_MAJOR << 16) | AEROGPU_ABI_MINOR;

pub const fn abi_major(version_u32: u32) -> u16 {
    (version_u32 >> 16) as u16
}

pub const fn abi_minor(version_u32: u32) -> u16 {
    (version_u32 & 0xFFFF) as u16
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AerogpuAbiVersion {
    pub major: u16,
    pub minor: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AerogpuAbiError {
    UnsupportedMajor { found: u16 },
}

/// Parse an ABI version and validate it according to the protocol rules:
/// - reject unsupported major versions
/// - accept unknown minor versions (treat as extensions and ignore what we don't understand)
pub fn parse_and_validate_abi_version_u32(
    version_u32: u32,
) -> Result<AerogpuAbiVersion, AerogpuAbiError> {
    let major = abi_major(version_u32);
    let minor = abi_minor(version_u32);

    if major != AEROGPU_ABI_MAJOR as u16 {
        return Err(AerogpuAbiError::UnsupportedMajor { found: major });
    }

    Ok(AerogpuAbiVersion { major, minor })
}

/* -------------------------------- PCI IDs -------------------------------- */

pub const AEROGPU_PCI_VENDOR_ID: u16 = 0xA3A0;
pub const AEROGPU_PCI_DEVICE_ID: u16 = 0x0001;
pub const AEROGPU_PCI_SUBSYSTEM_VENDOR_ID: u16 = AEROGPU_PCI_VENDOR_ID;
pub const AEROGPU_PCI_SUBSYSTEM_ID: u16 = 0x0001;

pub const AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER: u8 = 0x03;
pub const AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE: u8 = 0x00;
pub const AEROGPU_PCI_PROG_IF: u8 = 0x00;

pub const AEROGPU_PCI_BAR0_INDEX: u32 = 0;
pub const AEROGPU_PCI_BAR0_SIZE_BYTES: u32 = 64 * 1024;

/* ------------------------------ MMIO registers ---------------------------- */

pub const AEROGPU_MMIO_REG_MAGIC: u32 = 0x0000;
pub const AEROGPU_MMIO_REG_ABI_VERSION: u32 = 0x0004;
pub const AEROGPU_MMIO_REG_FEATURES_LO: u32 = 0x0008;
pub const AEROGPU_MMIO_REG_FEATURES_HI: u32 = 0x000C;

pub const AEROGPU_MMIO_MAGIC: u32 = 0x5550_4741; // "AGPU" LE

pub const AEROGPU_FEATURE_FENCE_PAGE: u64 = 1u64 << 0;
pub const AEROGPU_FEATURE_CURSOR: u64 = 1u64 << 1;
pub const AEROGPU_FEATURE_SCANOUT: u64 = 1u64 << 2;
pub const AEROGPU_FEATURE_VBLANK: u64 = 1u64 << 3;
pub const AEROGPU_FEATURE_TRANSFER: u64 = 1u64 << 4;

pub const AEROGPU_MMIO_REG_RING_GPA_LO: u32 = 0x0100;
pub const AEROGPU_MMIO_REG_RING_GPA_HI: u32 = 0x0104;
pub const AEROGPU_MMIO_REG_RING_SIZE_BYTES: u32 = 0x0108;
pub const AEROGPU_MMIO_REG_RING_CONTROL: u32 = 0x010C;

pub const AEROGPU_RING_CONTROL_ENABLE: u32 = 1u32 << 0;
pub const AEROGPU_RING_CONTROL_RESET: u32 = 1u32 << 1;

pub const AEROGPU_MMIO_REG_FENCE_GPA_LO: u32 = 0x0120;
pub const AEROGPU_MMIO_REG_FENCE_GPA_HI: u32 = 0x0124;

pub const AEROGPU_MMIO_REG_COMPLETED_FENCE_LO: u32 = 0x0130;
pub const AEROGPU_MMIO_REG_COMPLETED_FENCE_HI: u32 = 0x0134;

pub const AEROGPU_MMIO_REG_DOORBELL: u32 = 0x0200;

pub const AEROGPU_MMIO_REG_IRQ_STATUS: u32 = 0x0300;
pub const AEROGPU_MMIO_REG_IRQ_ENABLE: u32 = 0x0304;
pub const AEROGPU_MMIO_REG_IRQ_ACK: u32 = 0x0308;

pub const AEROGPU_IRQ_FENCE: u32 = 1u32 << 0;
pub const AEROGPU_IRQ_SCANOUT_VBLANK: u32 = 1u32 << 1;
pub const AEROGPU_IRQ_ERROR: u32 = 1u32 << 31;

pub const AEROGPU_MMIO_REG_SCANOUT0_ENABLE: u32 = 0x0400;
pub const AEROGPU_MMIO_REG_SCANOUT0_WIDTH: u32 = 0x0404;
pub const AEROGPU_MMIO_REG_SCANOUT0_HEIGHT: u32 = 0x0408;
pub const AEROGPU_MMIO_REG_SCANOUT0_FORMAT: u32 = 0x040C;
pub const AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES: u32 = 0x0410;
pub const AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO: u32 = 0x0414;
pub const AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI: u32 = 0x0418;

pub const AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO: u32 = 0x0420;
pub const AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI: u32 = 0x0424;
pub const AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO: u32 = 0x0428;
pub const AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_HI: u32 = 0x042C;
pub const AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS: u32 = 0x0430;

pub const AEROGPU_MMIO_REG_CURSOR_ENABLE: u32 = 0x0500;
pub const AEROGPU_MMIO_REG_CURSOR_X: u32 = 0x0504;
pub const AEROGPU_MMIO_REG_CURSOR_Y: u32 = 0x0508;
pub const AEROGPU_MMIO_REG_CURSOR_HOT_X: u32 = 0x050C;
pub const AEROGPU_MMIO_REG_CURSOR_HOT_Y: u32 = 0x0510;
pub const AEROGPU_MMIO_REG_CURSOR_WIDTH: u32 = 0x0514;
pub const AEROGPU_MMIO_REG_CURSOR_HEIGHT: u32 = 0x0518;
pub const AEROGPU_MMIO_REG_CURSOR_FORMAT: u32 = 0x051C;
pub const AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO: u32 = 0x0520;
pub const AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI: u32 = 0x0524;
pub const AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES: u32 = 0x0528;

/* ---------------------------------- Enums -------------------------------- */

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AerogpuFormat {
    Invalid = 0,

    B8G8R8A8Unorm = 1,
    B8G8R8X8Unorm = 2,
    R8G8B8A8Unorm = 3,
    R8G8B8X8Unorm = 4,

    B5G6R5Unorm = 5,
    B5G5R5A1Unorm = 6,

    B8G8R8A8UnormSrgb = 7,
    B8G8R8X8UnormSrgb = 8,
    R8G8B8A8UnormSrgb = 9,
    R8G8B8X8UnormSrgb = 10,

    D24UnormS8Uint = 32,
    D32Float = 33,

    BC1RgbaUnorm = 64,
    BC1RgbaUnormSrgb = 65,
    BC2RgbaUnorm = 66,
    BC2RgbaUnormSrgb = 67,
    BC3RgbaUnorm = 68,
    BC3RgbaUnormSrgb = 69,
    BC7RgbaUnorm = 70,
    BC7RgbaUnormSrgb = 71,
}
