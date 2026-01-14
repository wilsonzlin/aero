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
    pc.memory.write_u32(entry0, 0xfee0_0000);
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
    assert_eq!(restored.memory.read_u32(entry0_2), 0xfee0_0000);
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
    assert!(
        !restored.xhci.as_ref().unwrap().borrow().irq_level(),
        "xHCI INTx should be suppressed while MSI-X is active"
    );
}

#[test]
fn pc_platform_xhci_msix_snapshot_restore_preserves_pending_bit_and_delivers_after_unmask() {
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

    // Enable BAR0 MMIO decode + bus mastering, and disable legacy INTx so delivery cannot fall
    // back to line-based interrupts.
    pci_enable_mmio(&mut pc, bdf);
    pci_enable_bus_mastering(&mut pc, bdf);
    let mut cmd = pci_cfg_read_u16(&mut pc, bdf, 0x04);
    cmd |= 1 << 10; // INTX_DISABLE
    pci_cfg_write_u16(&mut pc, bdf, 0x04, cmd);

    let bar0_base = pci_read_bar(&mut pc, bdf, XhciPciDevice::MMIO_BAR_INDEX).base;
    assert_ne!(bar0_base, 0);

    // Locate MSI-X capability + table/PBA offsets.
    let msix_cap = find_capability(&mut pc, bdf, PCI_CAP_ID_MSIX)
        .expect("xHCI should expose an MSI-X capability in PCI config space");
    let msix_base = u16::from(msix_cap);

    let table = pci_cfg_read_u32(&mut pc, bdf, msix_base + 0x04);
    let pba = pci_cfg_read_u32(&mut pc, bdf, msix_base + 0x08);
    assert_eq!(
        table & 0x7,
        0,
        "xHCI MSI-X table should live in BAR0 (BIR=0)"
    );
    assert_eq!(pba & 0x7, 0, "xHCI MSI-X PBA should live in BAR0 (BIR=0)");
    let table_offset = u64::from(table & !0x7);
    let pba_offset = u64::from(pba & !0x7);

    // Enable MSI-X and set Function Mask (bit 14) so a raised interrupt is latched into the PBA
    // rather than immediately delivered.
    let ctrl = pci_cfg_read_u16(&mut pc, bdf, msix_base + 0x02);
    pci_cfg_write_u16(&mut pc, bdf, msix_base + 0x02, ctrl | (1 << 15) | (1 << 14));

    // Program table entry 0: destination = BSP (APIC ID 0), vector = 0x66.
    let vector: u8 = 0x66;
    let entry0 = bar0_base + table_offset;
    pc.memory.write_u32(entry0, 0xfee0_0000);
    pc.memory.write_u32(entry0 + 0x4, 0);
    pc.memory.write_u32(entry0 + 0x8, u32::from(vector));
    pc.memory.write_u32(entry0 + 0xc, 0); // unmasked

    // Sync the canonical PCI config space into the xHCI model before raising an interrupt.
    pc.tick(0);

    assert_eq!(pc.interrupts.borrow().get_pending(), None);

    // Raise an interrupt while MSI-X is function-masked: no delivery, but the PBA pending bit must
    // be set.
    pc.xhci
        .as_ref()
        .expect("xHCI enabled")
        .borrow_mut()
        .raise_event_interrupt();

    assert_eq!(
        pc.interrupts.borrow().get_pending(),
        None,
        "expected no MSI-X delivery while Function Mask is set"
    );
    assert!(
        !pc.xhci.as_ref().unwrap().borrow().irq_level(),
        "xHCI INTx should be suppressed while MSI-X is active (even if masked)"
    );
    let pba_bits = pc.memory.read_u64(bar0_base + pba_offset);
    assert_ne!(
        pba_bits & 1,
        0,
        "expected MSI-X pending bit 0 to be set while Function Mask is set"
    );

    // Snapshot xHCI + PCI core state while the pending bit is set.
    let xhci_snap = pc.xhci.as_ref().unwrap().borrow().save_state();
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

    let bar0_base2 = pci_read_bar(&mut restored, bdf, XhciPciDevice::MMIO_BAR_INDEX).base;
    assert_ne!(bar0_base2, 0);
    let msix_cap2 = find_capability(&mut restored, bdf, PCI_CAP_ID_MSIX)
        .expect("restored xHCI should still expose MSI-X");
    let msix_base2 = u16::from(msix_cap2);
    let pba2 = pci_cfg_read_u32(&mut restored, bdf, msix_base2 + 0x08);
    assert_eq!(pba2 & 0x7, 0, "xHCI MSI-X PBA should live in BAR0 (BIR=0)");
    let pba_offset2 = u64::from(pba2 & !0x7);

    // Pending bit should survive restore.
    let pba_bits2 = restored.memory.read_u64(bar0_base2 + pba_offset2);
    assert_ne!(
        pba_bits2 & 1,
        0,
        "expected MSI-X pending bit 0 to be preserved across snapshot/restore"
    );

    // Clear Function Mask in canonical PCI config space and sync into the device model.
    let ctrl2 = pci_cfg_read_u16(&mut restored, bdf, msix_base2 + 0x02);
    pci_cfg_write_u16(&mut restored, bdf, msix_base2 + 0x02, ctrl2 & !(1 << 14));
    restored.tick(0);

    // With the pending bit set and the interrupt condition still asserted, the next interrupt
    // service should deliver the MSI-X message and clear the pending bit.
    assert_eq!(restored.interrupts.borrow().get_pending(), None);
    restored
        .xhci
        .as_ref()
        .unwrap()
        .borrow_mut()
        .raise_event_interrupt();
    assert_eq!(restored.interrupts.borrow().get_pending(), Some(vector));

    let pba_bits_after = restored.memory.read_u64(bar0_base2 + pba_offset2);
    assert_eq!(
        pba_bits_after & 1,
        0,
        "expected MSI-X pending bit 0 to clear after unmask + delivery"
    );

    // Clear the LAPIC pending vector so the test is deterministic if additional interrupts fire.
    {
        let mut interrupts = restored.interrupts.borrow_mut();
        interrupts.acknowledge(vector);
        interrupts.eoi(vector);
    }
    assert_eq!(restored.interrupts.borrow().get_pending(), None);
}

#[test]
fn pc_platform_xhci_msix_snapshot_restore_preserves_vector_mask_pending_bit_and_delivers_after_unmask(
) {
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

    // Enable BAR0 MMIO decode + bus mastering, and disable legacy INTx so delivery cannot fall
    // back to line-based interrupts.
    pci_enable_mmio(&mut pc, bdf);
    pci_enable_bus_mastering(&mut pc, bdf);
    let mut cmd = pci_cfg_read_u16(&mut pc, bdf, 0x04);
    cmd |= 1 << 10; // INTX_DISABLE
    pci_cfg_write_u16(&mut pc, bdf, 0x04, cmd);

    let bar0_base = pci_read_bar(&mut pc, bdf, XhciPciDevice::MMIO_BAR_INDEX).base;
    assert_ne!(bar0_base, 0);

    // Locate MSI-X capability + table/PBA offsets.
    let msix_cap = find_capability(&mut pc, bdf, PCI_CAP_ID_MSIX)
        .expect("xHCI should expose an MSI-X capability in PCI config space");
    let msix_base = u16::from(msix_cap);
    let table = pci_cfg_read_u32(&mut pc, bdf, msix_base + 0x04);
    let pba = pci_cfg_read_u32(&mut pc, bdf, msix_base + 0x08);
    assert_eq!(
        table & 0x7,
        0,
        "xHCI MSI-X table should live in BAR0 (BIR=0)"
    );
    assert_eq!(pba & 0x7, 0, "xHCI MSI-X PBA should live in BAR0 (BIR=0)");
    let table_offset = u64::from(table & !0x7);
    let pba_offset = u64::from(pba & !0x7);

    // Enable MSI-X (bit 15) and ensure Function Mask (bit 14) is cleared.
    let ctrl = pci_cfg_read_u16(&mut pc, bdf, msix_base + 0x02);
    pci_cfg_write_u16(
        &mut pc,
        bdf,
        msix_base + 0x02,
        (ctrl & !(1 << 14)) | (1 << 15),
    );

    // Program table entry 0, but keep it masked (vector control bit 0).
    let vector: u8 = 0x67;
    let entry0 = bar0_base + table_offset;
    pc.memory.write_u32(entry0, 0xfee0_0000);
    pc.memory.write_u32(entry0 + 0x4, 0);
    pc.memory.write_u32(entry0 + 0x8, u32::from(vector));
    pc.memory.write_u32(entry0 + 0xc, 1); // entry masked

    // Sync the canonical PCI config space into the xHCI model before raising an interrupt.
    pc.tick(0);

    assert_eq!(pc.interrupts.borrow().get_pending(), None);

    // Raise an interrupt while the MSI-X table entry is masked: no delivery, but the PBA pending
    // bit must latch.
    pc.xhci
        .as_ref()
        .expect("xHCI enabled")
        .borrow_mut()
        .raise_event_interrupt();

    assert_eq!(
        pc.interrupts.borrow().get_pending(),
        None,
        "expected no MSI-X delivery while the MSI-X entry is masked"
    );
    assert!(
        !pc.xhci.as_ref().unwrap().borrow().irq_level(),
        "xHCI INTx should be suppressed while MSI-X is active (even if masked)"
    );
    assert_ne!(
        pc.memory.read_u64(bar0_base + pba_offset) & 1,
        0,
        "expected MSI-X pending bit 0 to be set while the entry is masked"
    );

    // Clear the interrupt condition before snapshot/unmask. Pending delivery should still occur
    // once the entry becomes unmasked (driven by the PBA pending bit rather than a new edge).
    pc.xhci
        .as_ref()
        .unwrap()
        .borrow_mut()
        .clear_event_interrupt();
    assert_ne!(
        pc.memory.read_u64(bar0_base + pba_offset) & 1,
        0,
        "expected MSI-X pending bit 0 to remain set after clearing the interrupt condition"
    );

    // Snapshot xHCI + PCI core state while the pending bit is set.
    let xhci_snap = pc.xhci.as_ref().unwrap().borrow().save_state();
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

    // Tick once to mirror MSI-X enable state into the xHCI model.
    restored.tick(0);

    let bar0_base2 = pci_read_bar(&mut restored, bdf, XhciPciDevice::MMIO_BAR_INDEX).base;
    assert_ne!(bar0_base2, 0);
    let msix_cap2 = find_capability(&mut restored, bdf, PCI_CAP_ID_MSIX)
        .expect("restored xHCI should still expose MSI-X");
    let msix_base2 = u16::from(msix_cap2);
    let table2 = pci_cfg_read_u32(&mut restored, bdf, msix_base2 + 0x04);
    let pba2 = pci_cfg_read_u32(&mut restored, bdf, msix_base2 + 0x08);
    assert_eq!(
        table2 & 0x7,
        0,
        "xHCI MSI-X table should live in BAR0 (BIR=0)"
    );
    assert_eq!(pba2 & 0x7, 0, "xHCI MSI-X PBA should live in BAR0 (BIR=0)");
    let table_offset2 = u64::from(table2 & !0x7);
    let pba_offset2 = u64::from(pba2 & !0x7);

    let entry0_2 = bar0_base2 + table_offset2;
    assert_eq!(
        restored.memory.read_u32(entry0_2 + 0xc) & 1,
        1,
        "expected MSI-X vector control mask bit to be restored"
    );
    assert_ne!(
        restored.memory.read_u64(bar0_base2 + pba_offset2) & 1,
        0,
        "expected MSI-X pending bit 0 to survive snapshot/restore"
    );
    assert_eq!(restored.interrupts.borrow().get_pending(), None);

    // Unmask after restore. This should immediately deliver the pending vector and clear the PBA.
    restored.memory.write_u32(entry0_2 + 0xc, 0);
    assert_eq!(restored.interrupts.borrow().get_pending(), Some(vector));
    assert_eq!(
        restored.memory.read_u64(bar0_base2 + pba_offset2) & 1,
        0,
        "expected MSI-X pending bit 0 to clear after restore + unmask + delivery"
    );
}

#[test]
fn pc_platform_xhci_msix_snapshot_restore_preserves_function_mask_pending_and_delivers_after_unmask_even_when_interrupt_cleared(
) {
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

    // Enable BAR0 MMIO decode + bus mastering, and keep legacy INTx enabled so we can assert there
    // is no INTx fallback while MSI-X is active.
    pci_enable_mmio(&mut pc, bdf);
    pci_enable_bus_mastering(&mut pc, bdf);
    let mut cmd = pci_cfg_read_u16(&mut pc, bdf, 0x04);
    cmd &= !(1 << 10); // INTX_DISABLE clear
    pci_cfg_write_u16(&mut pc, bdf, 0x04, cmd);

    let bar0_base = pci_read_bar(&mut pc, bdf, XhciPciDevice::MMIO_BAR_INDEX).base;
    assert_ne!(bar0_base, 0);

    // Locate MSI-X capability + table/PBA offsets.
    let msix_cap = find_capability(&mut pc, bdf, PCI_CAP_ID_MSIX)
        .expect("xHCI should expose an MSI-X capability in PCI config space");
    let msix_base = u16::from(msix_cap);
    let table = pci_cfg_read_u32(&mut pc, bdf, msix_base + 0x04);
    let pba = pci_cfg_read_u32(&mut pc, bdf, msix_base + 0x08);
    assert_eq!(
        table & 0x7,
        0,
        "xHCI MSI-X table should live in BAR0 (BIR=0)"
    );
    assert_eq!(pba & 0x7, 0, "xHCI MSI-X PBA should live in BAR0 (BIR=0)");
    let table_offset = u64::from(table & !0x7);
    let pba_offset = u64::from(pba & !0x7);

    // Enable MSI-X and set Function Mask (bit 14).
    let ctrl = pci_cfg_read_u16(&mut pc, bdf, msix_base + 0x02);
    pci_cfg_write_u16(
        &mut pc,
        bdf,
        msix_base + 0x02,
        ctrl | (1 << 15) | (1 << 14),
    );

    // Program table entry 0 (unmasked): destination = BSP (APIC ID 0), vector = 0x6c.
    let vector: u8 = 0x6c;
    let entry0 = bar0_base + table_offset;
    pc.memory.write_u32(entry0, 0xfee0_0000);
    pc.memory.write_u32(entry0 + 0x4, 0);
    pc.memory.write_u32(entry0 + 0x8, u32::from(vector));
    pc.memory.write_u32(entry0 + 0xc, 0);

    // Sync the canonical PCI config space into the xHCI model before raising an interrupt.
    pc.tick(0);

    assert_eq!(pc.interrupts.borrow().get_pending(), None);

    // Raise an interrupt while function-masked: no delivery, but PBA pending bit must latch.
    pc.xhci
        .as_ref()
        .expect("xHCI enabled")
        .borrow_mut()
        .raise_event_interrupt();

    assert_eq!(
        pc.interrupts.borrow().get_pending(),
        None,
        "expected no MSI-X delivery while Function Mask is set"
    );
    assert!(
        !pc.xhci.as_ref().unwrap().borrow().irq_level(),
        "xHCI INTx should be suppressed while MSI-X is active (even if function-masked)"
    );
    assert_ne!(
        pc.memory.read_u64(bar0_base + pba_offset) & 1,
        0,
        "expected MSI-X pending bit 0 to be set while function-masked"
    );

    // Clear the interrupt condition before snapshot/unmask. Pending delivery should still occur
    // once the function mask is cleared.
    pc.xhci.as_ref().unwrap().borrow_mut().clear_event_interrupt();
    assert_ne!(
        pc.memory.read_u64(bar0_base + pba_offset) & 1,
        0,
        "expected MSI-X pending bit 0 to remain set after clearing the interrupt condition"
    );

    // Snapshot xHCI + PCI core state while the pending bit is set.
    let xhci_snap = pc.xhci.as_ref().unwrap().borrow().save_state();
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

    let bar0_base2 = pci_read_bar(&mut restored, bdf, XhciPciDevice::MMIO_BAR_INDEX).base;
    assert_ne!(bar0_base2, 0);
    let msix_cap2 = find_capability(&mut restored, bdf, PCI_CAP_ID_MSIX)
        .expect("restored xHCI should still expose MSI-X");
    let msix_base2 = u16::from(msix_cap2);
    let pba2 = pci_cfg_read_u32(&mut restored, bdf, msix_base2 + 0x08);
    assert_eq!(pba2 & 0x7, 0, "xHCI MSI-X PBA should live in BAR0 (BIR=0)");
    let pba_offset2 = u64::from(pba2 & !0x7);

    // Pending bit should survive restore.
    assert_ne!(
        restored.memory.read_u64(bar0_base2 + pba_offset2) & 1,
        0,
        "expected MSI-X pending bit 0 to be preserved across snapshot/restore"
    );
    assert_eq!(restored.interrupts.borrow().get_pending(), None);

    // Clear Function Mask in canonical PCI config space and sync into the device model.
    let ctrl2 = pci_cfg_read_u16(&mut restored, bdf, msix_base2 + 0x02);
    pci_cfg_write_u16(&mut restored, bdf, msix_base2 + 0x02, ctrl2 & !(1 << 14));
    restored.tick(0);

    // Call `clear_event_interrupt` again (with the interrupt condition already cleared) to force a
    // service pass that drains the MSI-X PBA pending bit. This should deliver the vector even
    // without a new interrupt edge.
    restored
        .xhci
        .as_ref()
        .unwrap()
        .borrow_mut()
        .clear_event_interrupt();
    assert_eq!(restored.interrupts.borrow().get_pending(), Some(vector));
    assert_eq!(
        restored.memory.read_u64(bar0_base2 + pba_offset2) & 1,
        0,
        "expected MSI-X pending bit 0 to clear after function unmask + delivery"
    );

    // Clear the LAPIC pending vector so the test is deterministic if additional interrupts fire.
    {
        let mut interrupts = restored.interrupts.borrow_mut();
        interrupts.acknowledge(vector);
        interrupts.eoi(vector);
    }
    assert_eq!(restored.interrupts.borrow().get_pending(), None);
}
