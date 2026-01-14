use aero_protocol::aerogpu::aerogpu_pci as pci;

// Constants mirrored from `drivers/aerogpu/protocol/aerogpu_pci.h` via `aero-protocol`.

pub use pci::{
    AEROGPU_ABI_MAJOR, AEROGPU_ABI_MINOR, AEROGPU_ABI_VERSION_U32,
    AEROGPU_FEATURE_CURSOR as FEATURE_CURSOR, AEROGPU_FEATURE_FENCE_PAGE as FEATURE_FENCE_PAGE,
    AEROGPU_FEATURE_SCANOUT as FEATURE_SCANOUT, AEROGPU_FEATURE_TRANSFER as FEATURE_TRANSFER,
    AEROGPU_FEATURE_VBLANK as FEATURE_VBLANK, AEROGPU_MMIO_MAGIC,
    AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER, AEROGPU_PCI_DEVICE_ID, AEROGPU_PCI_PROG_IF,
    AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE, AEROGPU_PCI_SUBSYSTEM_ID, AEROGPU_PCI_SUBSYSTEM_VENDOR_ID,
    AEROGPU_PCI_VENDOR_ID, AerogpuErrorCode,
};

pub const AEROGPU_PCI_BAR0_SIZE_BYTES: u64 = pci::AEROGPU_PCI_BAR0_SIZE_BYTES as u64;

// Re-export scanout types so `AeroGpuRegs` and the scanout helpers share a single type family.
pub use crate::scanout::{AeroGpuCursorConfig, AeroGpuFormat, AeroGpuScanoutConfig};

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

    // Error reporting (ABI 1.3+).
    pub const ERROR_CODE: u64 = pci::AEROGPU_MMIO_REG_ERROR_CODE as u64;
    pub const ERROR_FENCE_LO: u64 = pci::AEROGPU_MMIO_REG_ERROR_FENCE_LO as u64;
    pub const ERROR_FENCE_HI: u64 = pci::AEROGPU_MMIO_REG_ERROR_FENCE_HI as u64;
    pub const ERROR_COUNT: u64 = pci::AEROGPU_MMIO_REG_ERROR_COUNT as u64;

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

    /// Most recent error code written to `MMIO_REG_ERROR_CODE` (ABI 1.3+).
    pub error_code: u32,
    /// Fence associated with the most recent error (ABI 1.3+).
    pub error_fence: u64,
    /// Monotonic count of errors recorded (ABI 1.3+).
    pub error_count: u32,

    pub current_submission_fence: u64,

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
            error_code: AerogpuErrorCode::None as u32,
            error_fence: 0,
            error_count: 0,
            current_submission_fence: 0,
            scanout0: AeroGpuScanoutConfig::default(),
            scanout0_vblank_seq: 0,
            scanout0_vblank_time_ns: 0,
            scanout0_vblank_period_ns: 0,
            cursor: AeroGpuCursorConfig::default(),
            stats: AeroGpuStats::default(),
        }
    }
}

impl AeroGpuRegs {
    pub fn record_error(&mut self, code: AerogpuErrorCode, fence: u64) {
        self.error_code = code as u32;
        self.error_fence = fence;
        self.error_count = self.error_count.saturating_add(1);
        self.irq_status |= irq_bits::ERROR;
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

    #[test]
    fn record_error_latches_error_fields_and_irq() {
        let mut regs = AeroGpuRegs::default();
        assert_eq!(regs.error_code, AerogpuErrorCode::None as u32);
        assert_eq!(regs.error_fence, 0);
        assert_eq!(regs.error_count, 0);
        assert_eq!(regs.irq_status & irq_bits::ERROR, 0);

        regs.record_error(AerogpuErrorCode::CmdDecode, 123);
        assert_eq!(regs.error_code, AerogpuErrorCode::CmdDecode as u32);
        assert_eq!(regs.error_fence, 123);
        assert_eq!(regs.error_count, 1);
        assert_ne!(regs.irq_status & irq_bits::ERROR, 0);

        regs.record_error(AerogpuErrorCode::Oob, 456);
        assert_eq!(regs.error_code, AerogpuErrorCode::Oob as u32);
        assert_eq!(regs.error_fence, 456);
        assert_eq!(regs.error_count, 2);
        assert_ne!(regs.irq_status & irq_bits::ERROR, 0);
    }
}
