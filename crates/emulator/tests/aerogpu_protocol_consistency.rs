use aero_protocol::aerogpu::{aerogpu_pci, aerogpu_ring};

use emulator::devices::{aerogpu_regs, aerogpu_ring as emu_ring};

#[test]
fn a3a0_protocol_constants_match_aero_protocol_crate() {
    assert_eq!(
        aerogpu_regs::AEROGPU_PCI_VENDOR_ID,
        aerogpu_pci::AEROGPU_PCI_VENDOR_ID
    );
    assert_eq!(
        aerogpu_regs::AEROGPU_PCI_DEVICE_ID,
        aerogpu_pci::AEROGPU_PCI_DEVICE_ID
    );
    assert_eq!(
        aerogpu_regs::AEROGPU_PCI_SUBSYSTEM_VENDOR_ID,
        aerogpu_pci::AEROGPU_PCI_SUBSYSTEM_VENDOR_ID
    );
    assert_eq!(
        aerogpu_regs::AEROGPU_PCI_SUBSYSTEM_ID,
        aerogpu_pci::AEROGPU_PCI_SUBSYSTEM_ID
    );
    assert_eq!(
        aerogpu_regs::AEROGPU_PCI_BAR0_SIZE_BYTES as u32,
        aerogpu_pci::AEROGPU_PCI_BAR0_SIZE_BYTES
    );

    assert_eq!(
        aerogpu_regs::AEROGPU_MMIO_MAGIC,
        aerogpu_pci::AEROGPU_MMIO_MAGIC
    );
    assert_eq!(
        aerogpu_regs::mmio::MAGIC as u32,
        aerogpu_pci::AEROGPU_MMIO_REG_MAGIC
    );

    assert_eq!(
        emu_ring::AEROGPU_RING_MAGIC,
        aerogpu_ring::AEROGPU_RING_MAGIC
    );
    assert_eq!(
        emu_ring::AEROGPU_FENCE_PAGE_MAGIC,
        aerogpu_ring::AEROGPU_FENCE_PAGE_MAGIC
    );
}
