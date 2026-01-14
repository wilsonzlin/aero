mod helpers;

use aero_devices::pci::msi::PCI_CAP_ID_MSI;
use aero_devices::pci::msix::PCI_CAP_ID_MSIX;
use aero_devices::pci::profile::USB_XHCI_QEMU;
use aero_devices::pci::{MsiCapability, PciBdf, PciDevice};
use aero_devices::usb::xhci::XhciPciDevice;
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
    assert!(
        !pc.xhci.as_ref().unwrap().borrow().irq_level(),
        "xHCI INTx should be suppressed while MSI is active"
    );
}

#[test]
fn pc_platform_xhci_msi_masked_interrupt_sets_pending_and_redelivers_after_unmask() {
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

    // Locate and program MSI, but start with the vector masked.
    let msi_cap = find_capability(&mut pc, bdf, PCI_CAP_ID_MSI)
        .expect("xHCI should expose an MSI capability in PCI config space");
    let base = u16::from(msi_cap);

    let vector: u8 = 0x65;
    pci_cfg_write_u32(&mut pc, bdf, base + 0x04, 0xfee0_0000);
    pci_cfg_write_u32(&mut pc, bdf, base + 0x08, 0);
    pci_cfg_write_u16(&mut pc, bdf, base + 0x0c, u16::from(vector));
    pci_cfg_write_u32(&mut pc, bdf, base + 0x10, 1); // mask

    let ctrl = pci_cfg_read_u16(&mut pc, bdf, base + 0x02);
    let is_64bit = (ctrl & (1 << 7)) != 0;
    let per_vector_masking = (ctrl & (1 << 8)) != 0;
    assert!(per_vector_masking, "test requires per-vector masking support");
    let pending_off = if is_64bit { base + 0x14 } else { base + 0x10 };
    pci_cfg_write_u16(&mut pc, bdf, base + 0x02, ctrl | 1); // MSI enable

    // Sync the canonical MSI state into the device model before raising interrupts.
    pc.tick(0);

    assert_eq!(pc.interrupts.borrow().get_pending(), None);

    let xhci = pc.xhci.as_ref().expect("xHCI enabled").clone();
    xhci.borrow_mut().raise_event_interrupt();

    // MSI is masked; delivery should be suppressed.
    assert_eq!(pc.interrupts.borrow().get_pending(), None);

    // The xHCI model should have latched its pending bit.
    assert!(
        xhci.borrow()
            .config()
            .capability::<MsiCapability>()
            .is_some_and(|msi| (msi.pending_bits() & 1) != 0),
        "masked MSI should set the pending bit in the device model"
    );

    // Now unmask MSI in the canonical config space. This previously clobbered device-managed MSI
    // pending bits when the platform mirrored canonical PCI config into the device config image.
    pci_cfg_write_u32(&mut pc, bdf, base + 0x10, 0); // unmask
    pc.tick(0);

    // Pending bit should still be set inside the device model and must be visible via the MSI
    // Pending Bits register in PCI config space.
    assert!(
        xhci.borrow()
            .config()
            .capability::<MsiCapability>()
            .is_some_and(|msi| (msi.pending_bits() & 1) != 0),
        "canonical PCI config sync must not clear device-managed MSI pending bits"
    );
    assert_ne!(
        pci_cfg_read_u32(&mut pc, bdf, pending_off) & 1,
        0,
        "expected MSI pending bit to be guest-visible via canonical PCI config space reads"
    );

    // Re-drive the interrupt condition; the device model should re-trigger MSI due to the pending
    // bit even though there's no new rising edge.
    xhci.borrow_mut().raise_event_interrupt();
    assert_eq!(pc.interrupts.borrow().get_pending(), Some(vector));

    // Sync once more so the canonical PCI config space reflects the pending-bit clear.
    pc.tick(0);
    assert_eq!(
        pci_cfg_read_u32(&mut pc, bdf, pending_off) & 1,
        0,
        "expected MSI pending bit to clear after delivery"
    );
}

#[test]
fn pc_platform_xhci_msix_triggers_lapic_vector_and_suppresses_intx() {
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

    // Switch into APIC mode so MSI-X delivery reaches the LAPIC.
    pc.io.write_u8(IMCR_SELECT_PORT, IMCR_INDEX);
    pc.io.write_u8(IMCR_DATA_PORT, 0x01);
    assert_eq!(pc.interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    // Enable BAR0 MMIO decode + bus mastering, and disable legacy INTx so MSI-X delivery is required
    // for interrupts to be observed.
    pci_enable_mmio(&mut pc, bdf);
    pci_enable_bus_mastering(&mut pc, bdf);
    let mut cmd = pci_cfg_read_u16(&mut pc, bdf, 0x04);
    cmd |= 1 << 10; // INTX_DISABLE
    pci_cfg_write_u16(&mut pc, bdf, 0x04, cmd);

    let bar0_base = pci_read_bar(&mut pc, bdf, XhciPciDevice::MMIO_BAR_INDEX).base;
    assert_ne!(bar0_base, 0);

    // Locate MSI-X capability.
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

    // Enable MSI-X.
    let ctrl = pci_cfg_read_u16(&mut pc, bdf, msix_base + 0x02);
    pci_cfg_write_u16(&mut pc, bdf, msix_base + 0x02, ctrl | (1 << 15));
    // Program table entry 0: destination = BSP (APIC ID 0), vector = 0x65.
    let entry0 = bar0_base + table_offset;
    pc.memory.write_u32(entry0, 0xfee0_0000);
    pc.memory.write_u32(entry0 + 0x4, 0);
    pc.memory.write_u32(entry0 + 0x8, 0x0065);
    pc.memory.write_u32(entry0 + 0xc, 0); // unmasked

    // Tick once to mirror MSI-X state into the xHCI model before triggering an interrupt.
    pc.tick(0);

    assert_eq!(pc.interrupts.borrow().get_pending(), None);

    pc.xhci
        .as_ref()
        .expect("xHCI should be enabled")
        .borrow_mut()
        .raise_event_interrupt();

    assert_eq!(pc.interrupts.borrow().get_pending(), Some(0x65));
    assert!(
        !pc.xhci.as_ref().unwrap().borrow().irq_level(),
        "xHCI INTx should be suppressed while MSI-X is active"
    );
}
