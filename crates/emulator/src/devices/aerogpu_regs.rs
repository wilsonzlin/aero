//! AeroGPU register block + MMIO constants.
//!
//! The emulator crate carries a legacy PCI wrapper (`devices::pci::aerogpu`) and GPU worker
//! infrastructure, but the device-side register definitions are canonical in `aero-devices-gpu`.

pub use aero_devices_gpu::regs::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regs_types_are_reexported_from_aero_devices_gpu() {
        let _: aero_devices_gpu::regs::AeroGpuRegs = AeroGpuRegs::default();
        let _: aero_devices_gpu::regs::AeroGpuStats = AeroGpuStats::default();
    }
}
