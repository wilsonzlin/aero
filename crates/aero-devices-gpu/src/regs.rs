use aero_protocol::aerogpu::aerogpu_pci as pci;

// Constants mirrored from `drivers/aerogpu/protocol/aerogpu_pci.h` via `aero-protocol`.

pub use pci::{
    AEROGPU_ABI_MAJOR, AEROGPU_ABI_MINOR, AEROGPU_ABI_VERSION_U32,
    AEROGPU_FEATURE_CURSOR as FEATURE_CURSOR, AEROGPU_FEATURE_FENCE_PAGE as FEATURE_FENCE_PAGE,
    AEROGPU_FEATURE_SCANOUT as FEATURE_SCANOUT, AEROGPU_FEATURE_TRANSFER as FEATURE_TRANSFER,
    AEROGPU_FEATURE_VBLANK as FEATURE_VBLANK, AEROGPU_MMIO_MAGIC,
    AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER, AEROGPU_PCI_DEVICE_ID, AEROGPU_PCI_PROG_IF,
    AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE, AEROGPU_PCI_SUBSYSTEM_ID, AEROGPU_PCI_SUBSYSTEM_VENDOR_ID,
    AEROGPU_PCI_VENDOR_ID,
};

pub const AEROGPU_PCI_BAR0_SIZE_BYTES: u64 = pci::AEROGPU_PCI_BAR0_SIZE_BYTES as u64;

pub const SUPPORTED_FEATURES: u64 = FEATURE_FENCE_PAGE
    | FEATURE_CURSOR
    | FEATURE_SCANOUT
    | FEATURE_VBLANK
    | if AEROGPU_ABI_MINOR >= 1 {
        FEATURE_TRANSFER
    } else {
        0
    };

pub mod mmio {
    use aero_protocol::aerogpu::aerogpu_pci as pci;

    pub const MAGIC: u64 = pci::AEROGPU_MMIO_REG_MAGIC as u64;
    pub const ABI_VERSION: u64 = pci::AEROGPU_MMIO_REG_ABI_VERSION as u64;
    pub const FEATURES_LO: u64 = pci::AEROGPU_MMIO_REG_FEATURES_LO as u64;
    pub const FEATURES_HI: u64 = pci::AEROGPU_MMIO_REG_FEATURES_HI as u64;

    pub const RING_GPA_LO: u64 = pci::AEROGPU_MMIO_REG_RING_GPA_LO as u64;
    pub const RING_GPA_HI: u64 = pci::AEROGPU_MMIO_REG_RING_GPA_HI as u64;
    pub const RING_SIZE_BYTES: u64 = pci::AEROGPU_MMIO_REG_RING_SIZE_BYTES as u64;
    pub const RING_CONTROL: u64 = pci::AEROGPU_MMIO_REG_RING_CONTROL as u64;

    pub const FENCE_GPA_LO: u64 = pci::AEROGPU_MMIO_REG_FENCE_GPA_LO as u64;
    pub const FENCE_GPA_HI: u64 = pci::AEROGPU_MMIO_REG_FENCE_GPA_HI as u64;

    pub const COMPLETED_FENCE_LO: u64 = pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO as u64;
    pub const COMPLETED_FENCE_HI: u64 = pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI as u64;

    pub const DOORBELL: u64 = pci::AEROGPU_MMIO_REG_DOORBELL as u64;

    pub const IRQ_STATUS: u64 = pci::AEROGPU_MMIO_REG_IRQ_STATUS as u64;
    pub const IRQ_ENABLE: u64 = pci::AEROGPU_MMIO_REG_IRQ_ENABLE as u64;
    pub const IRQ_ACK: u64 = pci::AEROGPU_MMIO_REG_IRQ_ACK as u64;

    pub const SCANOUT0_ENABLE: u64 = pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE as u64;
    pub const SCANOUT0_WIDTH: u64 = pci::AEROGPU_MMIO_REG_SCANOUT0_WIDTH as u64;
    pub const SCANOUT0_HEIGHT: u64 = pci::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT as u64;
    pub const SCANOUT0_FORMAT: u64 = pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT as u64;
    pub const SCANOUT0_PITCH_BYTES: u64 = pci::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES as u64;
    pub const SCANOUT0_FB_GPA_LO: u64 = pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO as u64;
    pub const SCANOUT0_FB_GPA_HI: u64 = pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI as u64;

    pub const SCANOUT0_VBLANK_SEQ_LO: u64 = pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO as u64;
    pub const SCANOUT0_VBLANK_SEQ_HI: u64 = pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI as u64;
    pub const SCANOUT0_VBLANK_TIME_NS_LO: u64 =
        pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO as u64;
    pub const SCANOUT0_VBLANK_TIME_NS_HI: u64 =
        pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_HI as u64;
    pub const SCANOUT0_VBLANK_PERIOD_NS: u64 =
        pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS as u64;

    pub const CURSOR_ENABLE: u64 = pci::AEROGPU_MMIO_REG_CURSOR_ENABLE as u64;
    pub const CURSOR_X: u64 = pci::AEROGPU_MMIO_REG_CURSOR_X as u64;
    pub const CURSOR_Y: u64 = pci::AEROGPU_MMIO_REG_CURSOR_Y as u64;
    pub const CURSOR_HOT_X: u64 = pci::AEROGPU_MMIO_REG_CURSOR_HOT_X as u64;
    pub const CURSOR_HOT_Y: u64 = pci::AEROGPU_MMIO_REG_CURSOR_HOT_Y as u64;
    pub const CURSOR_WIDTH: u64 = pci::AEROGPU_MMIO_REG_CURSOR_WIDTH as u64;
    pub const CURSOR_HEIGHT: u64 = pci::AEROGPU_MMIO_REG_CURSOR_HEIGHT as u64;
    pub const CURSOR_FORMAT: u64 = pci::AEROGPU_MMIO_REG_CURSOR_FORMAT as u64;
    pub const CURSOR_FB_GPA_LO: u64 = pci::AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO as u64;
    pub const CURSOR_FB_GPA_HI: u64 = pci::AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI as u64;
    pub const CURSOR_PITCH_BYTES: u64 = pci::AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES as u64;
}

pub mod ring_control {
    use aero_protocol::aerogpu::aerogpu_pci as pci;

    pub const ENABLE: u32 = pci::AEROGPU_RING_CONTROL_ENABLE;
    pub const RESET: u32 = pci::AEROGPU_RING_CONTROL_RESET;
}

pub mod irq_bits {
    use aero_protocol::aerogpu::aerogpu_pci as pci;

    pub const FENCE: u32 = pci::AEROGPU_IRQ_FENCE;
    pub const SCANOUT_VBLANK: u32 = pci::AEROGPU_IRQ_SCANOUT_VBLANK;
    pub const ERROR: u32 = pci::AEROGPU_IRQ_ERROR;
}

// Values derived from the canonical `aero-protocol` definition of `enum aerogpu_format`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum AeroGpuFormat {
    Invalid = pci::AerogpuFormat::Invalid as u32,
    B8G8R8A8Unorm = pci::AerogpuFormat::B8G8R8A8Unorm as u32,
    B8G8R8X8Unorm = pci::AerogpuFormat::B8G8R8X8Unorm as u32,
    R8G8B8A8Unorm = pci::AerogpuFormat::R8G8B8A8Unorm as u32,
    R8G8B8X8Unorm = pci::AerogpuFormat::R8G8B8X8Unorm as u32,
    B5G6R5Unorm = pci::AerogpuFormat::B5G6R5Unorm as u32,
    B5G5R5A1Unorm = pci::AerogpuFormat::B5G5R5A1Unorm as u32,
    B8G8R8A8UnormSrgb = pci::AerogpuFormat::B8G8R8A8UnormSrgb as u32,
    B8G8R8X8UnormSrgb = pci::AerogpuFormat::B8G8R8X8UnormSrgb as u32,
    R8G8B8A8UnormSrgb = pci::AerogpuFormat::R8G8B8A8UnormSrgb as u32,
    R8G8B8X8UnormSrgb = pci::AerogpuFormat::R8G8B8X8UnormSrgb as u32,
    D24UnormS8Uint = pci::AerogpuFormat::D24UnormS8Uint as u32,
    D32Float = pci::AerogpuFormat::D32Float as u32,
    // The scanout/cursor paths do not currently support BC formats, but we keep them representable
    // so the software executor can compute backing sizes (and ignore them when presenting).
    Bc1Unorm = pci::AerogpuFormat::BC1RgbaUnorm as u32,
    Bc1UnormSrgb = pci::AerogpuFormat::BC1RgbaUnormSrgb as u32,
    Bc2Unorm = pci::AerogpuFormat::BC2RgbaUnorm as u32,
    Bc2UnormSrgb = pci::AerogpuFormat::BC2RgbaUnormSrgb as u32,
    Bc3Unorm = pci::AerogpuFormat::BC3RgbaUnorm as u32,
    Bc3UnormSrgb = pci::AerogpuFormat::BC3RgbaUnormSrgb as u32,
    Bc7Unorm = pci::AerogpuFormat::BC7RgbaUnorm as u32,
    Bc7UnormSrgb = pci::AerogpuFormat::BC7RgbaUnormSrgb as u32,
}

#[derive(Clone, Debug)]
pub struct AeroGpuScanoutConfig {
    pub enable: bool,
    pub width: u32,
    pub height: u32,
    pub format: AeroGpuFormat,
    pub pitch_bytes: u32,
    pub fb_gpa: u64,
}

impl Default for AeroGpuScanoutConfig {
    fn default() -> Self {
        Self {
            enable: false,
            width: 0,
            height: 0,
            format: AeroGpuFormat::Invalid,
            pitch_bytes: 0,
            fb_gpa: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AeroGpuCursorConfig {
    pub enable: bool,
    pub x: i32,
    pub y: i32,
    pub hot_x: u32,
    pub hot_y: u32,
    pub width: u32,
    pub height: u32,
    pub format: AeroGpuFormat,
    pub fb_gpa: u64,
    pub pitch_bytes: u32,
}

impl Default for AeroGpuCursorConfig {
    fn default() -> Self {
        Self {
            enable: false,
            x: 0,
            y: 0,
            hot_x: 0,
            hot_y: 0,
            width: 0,
            height: 0,
            format: AeroGpuFormat::Invalid,
            fb_gpa: 0,
            pitch_bytes: 0,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct AeroGpuStats {
    pub doorbells: u64,
    pub submissions: u64,
    pub malformed_submissions: u64,
    pub gpu_exec_errors: u64,
}

#[derive(Clone, Debug)]
pub struct AeroGpuRegs {
    pub abi_version: u32,
    pub features: u64,

    pub ring_gpa: u64,
    pub ring_size_bytes: u32,
    pub ring_control: u32,

    pub fence_gpa: u64,
    pub completed_fence: u64,

    pub irq_status: u32,
    pub irq_enable: u32,

    pub scanout0: AeroGpuScanoutConfig,
    pub scanout0_vblank_seq: u64,
    pub scanout0_vblank_time_ns: u64,
    pub scanout0_vblank_period_ns: u32,
    pub cursor: AeroGpuCursorConfig,

    pub stats: AeroGpuStats,
}

impl Default for AeroGpuRegs {
    fn default() -> Self {
        Self {
            abi_version: AEROGPU_ABI_VERSION_U32,
            features: SUPPORTED_FEATURES,
            ring_gpa: 0,
            ring_size_bytes: 0,
            ring_control: 0,
            fence_gpa: 0,
            completed_fence: 0,
            irq_status: 0,
            irq_enable: 0,
            scanout0: AeroGpuScanoutConfig::default(),
            scanout0_vblank_seq: 0,
            scanout0_vblank_time_ns: 0,
            scanout0_vblank_period_ns: 0,
            cursor: AeroGpuCursorConfig::default(),
            stats: AeroGpuStats::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_abi_version_is_protocol_version() {
        assert_eq!(AeroGpuRegs::default().abi_version, AEROGPU_ABI_VERSION_U32);
    }

    #[test]
    fn supported_features_respect_abi_minor_version_gating() {
        assert_ne!(SUPPORTED_FEATURES & FEATURE_FENCE_PAGE, 0);
        assert_ne!(SUPPORTED_FEATURES & FEATURE_CURSOR, 0);
        assert_ne!(SUPPORTED_FEATURES & FEATURE_SCANOUT, 0);
        assert_ne!(SUPPORTED_FEATURES & FEATURE_VBLANK, 0);

        if AEROGPU_ABI_MINOR >= 1 {
            assert_ne!(SUPPORTED_FEATURES & FEATURE_TRANSFER, 0);
        } else {
            assert_eq!(SUPPORTED_FEATURES & FEATURE_TRANSFER, 0);
        }

        assert_eq!(AeroGpuRegs::default().features, SUPPORTED_FEATURES);
    }
}
