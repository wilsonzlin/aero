mod helpers;

use aero_devices::pci::msix::PCI_CAP_ID_MSIX;
use aero_devices::pci::profile::USB_XHCI_QEMU;
use aero_devices::pci::PciBdf;
use aero_devices::usb::xhci::XhciPciDevice;
use aero_io_snapshot::io::state::IoSnapshot;
use aero_pc_platform::{PcPlatform, PcPlatformConfig};
use aero_platform::interrupts::{
    InterruptController, PlatformInterruptMode, IMCR_DATA_PORT, IMCR_INDEX, IMCR_SELECT_PORT,
};
use helpers::*;
use memory::MemoryBus as _;

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
fn pc_platform_xhci_msix_snapshot_restore_preserves_table_and_delivery() {
    const RAM_SIZE: usize = 2 * 1024 * 1024;
    let config = PcPlatformConfig {
        enable_ahci: false,
        enable_uhci: false,
        enable_xhci: true,
        ..Default::default()
    };
    let mut pc = PcPlatform::new_with_config(RAM_SIZE, config);
    let bdf = USB_XHCI_QEMU.bdf;

    // Switch into APIC mode so MSI-X delivery reaches the LAPIC.
    pc.io.write_u8(IMCR_SELECT_PORT, IMCR_INDEX);
    pc.io.write_u8(IMCR_DATA_PORT, 0x01);
    assert_eq!(pc.interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    // Enable BAR0 MMIO decode + bus mastering, and disable legacy INTx so MSI-X delivery is required.
    pci_enable_mmio(&mut pc, bdf);
    pci_enable_bus_mastering(&mut pc, bdf);
    let mut cmd = pci_cfg_read_u16(&mut pc, bdf, 0x04);
    cmd |= 1 << 10; // INTX_DISABLE
    pci_cfg_write_u16(&mut pc, bdf, 0x04, cmd);

    let bar0_base = pci_read_bar(&mut pc, bdf, XhciPciDevice::MMIO_BAR_INDEX).base;
    assert_ne!(bar0_base, 0);

    // Locate + enable MSI-X.
    let msix_cap = find_capability(&mut pc, bdf, PCI_CAP_ID_MSIX)
        .expect("xHCI should expose an MSI-X capability in PCI config space");
    let msix_base = u16::from(msix_cap);

    // Table offset/BIR must point into BAR0.
    let table = pci_cfg_read_u32(&mut pc, bdf, msix_base + 0x04);
    assert_eq!(
        table & 0x7,
        0,
        "xHCI MSI-X table should live in BAR0 (BIR=0)"
    );
    let table_offset = u64::from(table & !0x7);

    let ctrl = pci_cfg_read_u16(&mut pc, bdf, msix_base + 0x02);
    pci_cfg_write_u16(&mut pc, bdf, msix_base + 0x02, ctrl | (1 << 15)); // MSI-X enable

    // Program table entry 0: destination = BSP (APIC ID 0), vector = 0x65.
    let entry0 = bar0_base + table_offset;
    pc.memory.write_u32(entry0 + 0x0, 0xfee0_0000);
    pc.memory.write_u32(entry0 + 0x4, 0);
    pc.memory.write_u32(entry0 + 0x8, 0x0065);
    pc.memory.write_u32(entry0 + 0xc, 0); // unmasked

    // Sync the canonical PCI config space into the xHCI model before snapshotting.
    pc.tick(0);

    // Snapshot xHCI + PCI core state.
    let xhci_snap = pc
        .xhci
        .as_ref()
        .expect("xHCI enabled")
        .borrow()
        .save_state();
    let pci_snap = pc.pci_cfg.borrow().save_state();

    // Restore into a fresh platform.
    let mut restored = PcPlatform::new_with_config(RAM_SIZE, config);
    restored.pci_cfg.borrow_mut().load_state(&pci_snap).unwrap();
    restored
        .xhci
        .as_ref()
        .expect("xHCI enabled")
        .borrow_mut()
        .load_state(&xhci_snap)
        .unwrap();

    // Switch restored platform to APIC mode (interrupt controller state is not part of these
    // manual snapshots).
    restored.io.write_u8(IMCR_SELECT_PORT, IMCR_INDEX);
    restored.io.write_u8(IMCR_DATA_PORT, 0x01);
    assert_eq!(
        restored.interrupts.borrow().mode(),
        PlatformInterruptMode::Apic
    );

    // Verify MSI-X enable bit is preserved in guest-visible PCI config space.
    let msix_cap2 = find_capability(&mut restored, bdf, PCI_CAP_ID_MSIX)
        .expect("restored xHCI should still expose MSI-X");
    let ctrl_restored = pci_cfg_read_u16(&mut restored, bdf, u16::from(msix_cap2) + 0x02);
    assert_ne!(
        ctrl_restored & (1 << 15),
        0,
        "MSI-X enable bit should be preserved across snapshot/restore"
    );

    // Verify MSI-X table contents are preserved.
    let bar0_base2 = pci_read_bar(&mut restored, bdf, XhciPciDevice::MMIO_BAR_INDEX).base;
    let table2 = pci_cfg_read_u32(&mut restored, bdf, u16::from(msix_cap2) + 0x04);
    assert_eq!(table2 & 0x7, 0, "xHCI MSI-X table should live in BAR0");
    let table_offset2 = u64::from(table2 & !0x7);
    let entry0_2 = bar0_base2 + table_offset2;
    assert_eq!(restored.memory.read_u32(entry0_2 + 0x0), 0xfee0_0000);
    assert_eq!(restored.memory.read_u32(entry0_2 + 0x4), 0);
    assert_eq!(restored.memory.read_u32(entry0_2 + 0x8), 0x0065);
    assert_eq!(restored.memory.read_u32(entry0_2 + 0xc) & 1, 0);

    // Tick once to mirror MSI-X state into the xHCI model before triggering an interrupt.
    restored.tick(0);

    assert_eq!(restored.interrupts.borrow().get_pending(), None);

    restored
        .xhci
        .as_ref()
        .unwrap()
        .borrow_mut()
        .raise_event_interrupt();

    assert_eq!(restored.interrupts.borrow().get_pending(), Some(0x65));
    assert_eq!(
        restored.xhci.as_ref().unwrap().borrow().irq_level(),
        false,
        "xHCI INTx should be suppressed while MSI-X is active"
    );
}
