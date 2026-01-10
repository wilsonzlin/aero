use crate::devices::aerogpu_scanout::{AeroGpuCursorConfig, AeroGpuScanoutConfig};

// Constants mirrored from `drivers/aerogpu/protocol/aerogpu_pci.h`.

pub const AEROGPU_ABI_MAJOR: u32 = 1;
pub const AEROGPU_ABI_MINOR: u32 = 0;
pub const AEROGPU_ABI_VERSION_U32: u32 = (AEROGPU_ABI_MAJOR << 16) | AEROGPU_ABI_MINOR;

pub const AEROGPU_PCI_VENDOR_ID: u16 = 0xA3A0;
pub const AEROGPU_PCI_DEVICE_ID: u16 = 0x0001;
pub const AEROGPU_PCI_SUBSYSTEM_VENDOR_ID: u16 = AEROGPU_PCI_VENDOR_ID;
pub const AEROGPU_PCI_SUBSYSTEM_ID: u16 = 0x0001;

pub const AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER: u8 = 0x03;
pub const AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE: u8 = 0x00;
pub const AEROGPU_PCI_PROG_IF: u8 = 0x00;

pub const AEROGPU_PCI_BAR0_SIZE_BYTES: u64 = 64 * 1024;

pub const AEROGPU_MMIO_MAGIC: u32 = 0x5550_4741; // "AGPU" little-endian

pub const FEATURE_FENCE_PAGE: u64 = 1u64 << 0;
pub const FEATURE_CURSOR: u64 = 1u64 << 1;
pub const FEATURE_SCANOUT: u64 = 1u64 << 2;
pub const FEATURE_VBLANK: u64 = 1u64 << 3;

pub const SUPPORTED_FEATURES: u64 = FEATURE_FENCE_PAGE | FEATURE_SCANOUT | FEATURE_VBLANK;

pub mod mmio {
    pub const MAGIC: u64 = 0x0000;
    pub const ABI_VERSION: u64 = 0x0004;
    pub const FEATURES_LO: u64 = 0x0008;
    pub const FEATURES_HI: u64 = 0x000c;

    pub const RING_GPA_LO: u64 = 0x0100;
    pub const RING_GPA_HI: u64 = 0x0104;
    pub const RING_SIZE_BYTES: u64 = 0x0108;
    pub const RING_CONTROL: u64 = 0x010c;

    pub const FENCE_GPA_LO: u64 = 0x0120;
    pub const FENCE_GPA_HI: u64 = 0x0124;

    pub const COMPLETED_FENCE_LO: u64 = 0x0130;
    pub const COMPLETED_FENCE_HI: u64 = 0x0134;

    pub const DOORBELL: u64 = 0x0200;

    pub const IRQ_STATUS: u64 = 0x0300;
    pub const IRQ_ENABLE: u64 = 0x0304;
    pub const IRQ_ACK: u64 = 0x0308;

    pub const SCANOUT0_ENABLE: u64 = 0x0400;
    pub const SCANOUT0_WIDTH: u64 = 0x0404;
    pub const SCANOUT0_HEIGHT: u64 = 0x0408;
    pub const SCANOUT0_FORMAT: u64 = 0x040c;
    pub const SCANOUT0_PITCH_BYTES: u64 = 0x0410;
    pub const SCANOUT0_FB_GPA_LO: u64 = 0x0414;
    pub const SCANOUT0_FB_GPA_HI: u64 = 0x0418;

    pub const SCANOUT0_VBLANK_SEQ_LO: u64 = 0x0420;
    pub const SCANOUT0_VBLANK_SEQ_HI: u64 = 0x0424;
    pub const SCANOUT0_VBLANK_TIME_NS_LO: u64 = 0x0428;
    pub const SCANOUT0_VBLANK_TIME_NS_HI: u64 = 0x042c;
    pub const SCANOUT0_VBLANK_PERIOD_NS: u64 = 0x0430;

    pub const CURSOR_ENABLE: u64 = 0x0500;
    pub const CURSOR_X: u64 = 0x0504;
    pub const CURSOR_Y: u64 = 0x0508;
    pub const CURSOR_HOT_X: u64 = 0x050c;
    pub const CURSOR_HOT_Y: u64 = 0x0510;
    pub const CURSOR_WIDTH: u64 = 0x0514;
    pub const CURSOR_HEIGHT: u64 = 0x0518;
    pub const CURSOR_FORMAT: u64 = 0x051c;
    pub const CURSOR_FB_GPA_LO: u64 = 0x0520;
    pub const CURSOR_FB_GPA_HI: u64 = 0x0524;
    pub const CURSOR_PITCH_BYTES: u64 = 0x0528;
}

pub mod ring_control {
    pub const ENABLE: u32 = 1 << 0;
    pub const RESET: u32 = 1 << 1;
}

pub mod irq_bits {
    pub const FENCE: u32 = 1 << 0;
    pub const SCANOUT_VBLANK: u32 = 1 << 1;
    pub const ERROR: u32 = 1u32 << 31;
}

#[derive(Clone, Debug, Default)]
pub struct AeroGpuStats {
    pub doorbells: u64,
    pub submissions: u64,
    pub malformed_submissions: u64,
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
