use crate::devices::aerogpu_scanout::{AeroGpuCursorConfig, AeroGpuScanoutConfig};

use aero_protocol::aerogpu::aerogpu_pci as pci;

// Constants mirrored from `drivers/aerogpu/protocol/aerogpu_pci.h` via `emulator/protocol`.

pub const AEROGPU_ABI_MAJOR: u32 = pci::AEROGPU_ABI_MAJOR;
pub const AEROGPU_ABI_MINOR: u32 = pci::AEROGPU_ABI_MINOR;
pub const AEROGPU_ABI_VERSION_U32: u32 = pci::AEROGPU_ABI_VERSION_U32;

pub const AEROGPU_PCI_VENDOR_ID: u16 = pci::AEROGPU_PCI_VENDOR_ID;
pub const AEROGPU_PCI_DEVICE_ID: u16 = pci::AEROGPU_PCI_DEVICE_ID;
pub const AEROGPU_PCI_SUBSYSTEM_VENDOR_ID: u16 = pci::AEROGPU_PCI_SUBSYSTEM_VENDOR_ID;
pub const AEROGPU_PCI_SUBSYSTEM_ID: u16 = pci::AEROGPU_PCI_SUBSYSTEM_ID;

pub const AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER: u8 = pci::AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER;
pub const AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE: u8 = pci::AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE;
pub const AEROGPU_PCI_PROG_IF: u8 = pci::AEROGPU_PCI_PROG_IF;

pub const AEROGPU_PCI_BAR0_SIZE_BYTES: u64 = pci::AEROGPU_PCI_BAR0_SIZE_BYTES as u64;

pub const AEROGPU_MMIO_MAGIC: u32 = pci::AEROGPU_MMIO_MAGIC;

pub const FEATURE_FENCE_PAGE: u64 = pci::AEROGPU_FEATURE_FENCE_PAGE;
pub const FEATURE_CURSOR: u64 = pci::AEROGPU_FEATURE_CURSOR;
pub const FEATURE_SCANOUT: u64 = pci::AEROGPU_FEATURE_SCANOUT;
pub const FEATURE_VBLANK: u64 = pci::AEROGPU_FEATURE_VBLANK;

pub const SUPPORTED_FEATURES: u64 = FEATURE_FENCE_PAGE | FEATURE_SCANOUT | FEATURE_VBLANK;

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
    pub const SCANOUT0_VBLANK_TIME_NS_LO: u64 = pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO as u64;
    pub const SCANOUT0_VBLANK_TIME_NS_HI: u64 = pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_HI as u64;
    pub const SCANOUT0_VBLANK_PERIOD_NS: u64 = pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS as u64;

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
