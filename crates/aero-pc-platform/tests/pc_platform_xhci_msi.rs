mod helpers;

use aero_devices::pci::msi::PCI_CAP_ID_MSI;
use aero_devices::pci::profile::USB_XHCI_QEMU;
use aero_devices::pci::PciBdf;
use aero_pc_platform::{PcPlatform, PcPlatformConfig};
use aero_platform::interrupts::{
    InterruptController, PlatformInterruptMode, IMCR_DATA_PORT, IMCR_INDEX, IMCR_SELECT_PORT,
};
use helpers::*;

fn find_capability(pc: &mut PcPlatform, bdf: PciBdf, id: u8) -> Option<u8> {
    let mut offset = pci_cfg_read_u8(pc, bdf, 0x34);
    for _ in 0..64 {
        if offset == 0 {
            return None;
        }
        let cap_id = pci_cfg_read_u8(pc, bdf, u16::from(offset));
        if cap_id == id {
            return Some(offset);
        }
        offset = pci_cfg_read_u8(pc, bdf, u16::from(offset) + 1);
    }
    None
}

#[test]
fn pc_platform_xhci_msi_triggers_lapic_vector_and_suppresses_intx() {
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            enable_ahci: false,
            enable_uhci: false,
            enable_xhci: true,
            ..Default::default()
        },
    );
    let bdf = USB_XHCI_QEMU.bdf;

    // Switch into APIC mode so MSI delivery reaches the LAPIC.
    pc.io.write_u8(IMCR_SELECT_PORT, IMCR_INDEX);
    pc.io.write_u8(IMCR_DATA_PORT, 0x01);
    assert_eq!(pc.interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    // Enable BAR0 MMIO decode + bus mastering.
    pci_enable_mmio(&mut pc, bdf);
    pci_enable_bus_mastering(&mut pc, bdf);

    // Locate and program the MSI capability: dest = BSP (APIC ID 0), vector = 0x65.
    let msi_cap = find_capability(&mut pc, bdf, PCI_CAP_ID_MSI)
        .expect("xHCI should expose an MSI capability in PCI config space");
    let base = u16::from(msi_cap);

    pci_cfg_write_u32(&mut pc, bdf, base + 0x04, 0xfee0_0000);
    pci_cfg_write_u32(&mut pc, bdf, base + 0x08, 0);
    pci_cfg_write_u16(&mut pc, bdf, base + 0x0c, 0x0065);
    pci_cfg_write_u32(&mut pc, bdf, base + 0x10, 0); // unmask

    let ctrl = pci_cfg_read_u16(&mut pc, bdf, base + 0x02);
    pci_cfg_write_u16(&mut pc, bdf, base + 0x02, ctrl | 1); // MSI enable

    // The PC platform owns the canonical config space, so tick once to mirror MSI state into the
    // xHCI model before triggering an interrupt.
    pc.tick(0);

    assert_eq!(pc.interrupts.borrow().get_pending(), None);

    pc.xhci
        .as_ref()
        .expect("xHCI should be enabled")
        .borrow_mut()
        .raise_event_interrupt();

    assert_eq!(pc.interrupts.borrow().get_pending(), Some(0x65));
    assert_eq!(
        pc.xhci.as_ref().unwrap().borrow().irq_level(),
        false,
        "xHCI INTx should be suppressed while MSI is active"
    );
}

