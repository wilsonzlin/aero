#![cfg(not(target_arch = "wasm32"))]

use aero_devices::pci::{profile, PciBdf, PciInterruptPin, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_machine::{Machine, MachineConfig};
use aero_platform::interrupts::{
    InterruptController, PlatformInterruptMode, IMCR_DATA_PORT, IMCR_INDEX, IMCR_SELECT_PORT,
};
use aero_virtio::queue::{VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE};
use pretty_assertions::{assert_eq, assert_ne};

fn cfg_addr(bdf: PciBdf, offset: u16) -> u32 {
    0x8000_0000
        | (u32::from(bdf.bus) << 16)
        | (u32::from(bdf.device & 0x1f) << 11)
        | (u32::from(bdf.function & 0x07) << 8)
        | (u32::from(offset) & 0xfc)
}

fn cfg_read(m: &mut Machine, bdf: PciBdf, offset: u16, size: u8) -> u32 {
    m.io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    m.io_read(PCI_CFG_DATA_PORT + (offset & 3), size)
}

fn cfg_write(m: &mut Machine, bdf: PciBdf, offset: u16, size: u8, value: u32) {
    m.io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    m.io_write(PCI_CFG_DATA_PORT + (offset & 3), size, value);
}

fn find_capability(m: &mut Machine, bdf: PciBdf, cap_id: u8) -> Option<u16> {
    let mut ptr = cfg_read(m, bdf, 0x34, 1) as u8;
    for _ in 0..64 {
        if ptr == 0 {
            return None;
        }
        let id = cfg_read(m, bdf, u16::from(ptr), 1) as u8;
        if id == cap_id {
            return Some(u16::from(ptr));
        }
        ptr = cfg_read(m, bdf, u16::from(ptr) + 1, 1) as u8;
    }
    None
}

fn write_desc(m: &mut Machine, table: u64, index: u16, addr: u64, len: u32, flags: u16, next: u16) {
    let base = table + u64::from(index) * 16;
    m.write_physical_u64(base, addr);
    m.write_physical_u32(base + 8, len);
    m.write_physical_u16(base + 12, flags);
    m.write_physical_u16(base + 14, next);
}

#[test]
fn virtio_blk_msix_delivers_to_lapic_in_apic_mode() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 4 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_blk: true,
        // Keep the test focused on PCI + virtio-blk.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    let virtio_blk = m.virtio_blk().expect("virtio-blk enabled");
    let bdf = profile::VIRTIO_BLK.bdf;

    // Switch into APIC mode so MSI delivery targets the LAPIC.
    m.io_write(IMCR_SELECT_PORT, 1, u32::from(IMCR_INDEX));
    m.io_write(IMCR_DATA_PORT, 1, 0x01);
    assert_eq!(interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    // Enable PCI memory decoding so BAR0 MMIO is reachable.
    let cmd = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(cmd | (1 << 1) | (1 << 2)));

    // Discover BAR0.
    let bar0_lo = cfg_read(&mut m, bdf, 0x10, 4) as u64;
    let bar0_hi = cfg_read(&mut m, bdf, 0x14, 4) as u64;
    let bar0_base = (bar0_hi << 32) | (bar0_lo & !0xFu64);
    assert_ne!(bar0_base, 0, "expected virtio-blk BAR0 to be assigned");

    // Locate MSI-X capability and validate table/PBA live in BAR0.
    let msix_cap = find_capability(&mut m, bdf, aero_devices::pci::msix::PCI_CAP_ID_MSIX)
        .expect("virtio-blk should expose MSI-X capability");
    let table = cfg_read(&mut m, bdf, msix_cap + 0x04, 4);
    let pba = cfg_read(&mut m, bdf, msix_cap + 0x08, 4);
    assert_eq!(table & 0x7, 0, "MSI-X table must live in BAR0 (BIR=0)");
    assert_eq!(pba & 0x7, 0, "MSI-X PBA must live in BAR0 (BIR=0)");

    // Program table entry 0 with an xAPIC message targeting vector 0x61.
    let vector = 0x61u32;
    let table_offset = u64::from(table & !0x7);
    let entry0 = bar0_base + table_offset;
    m.write_physical_u32(entry0, 0xfee0_0000);
    m.write_physical_u32(entry0 + 0x4, 0);
    m.write_physical_u32(entry0 + 0x8, vector);
    m.write_physical_u32(entry0 + 0xc, 0); // unmasked

    // Enable MSI-X (bit 15) and ensure function mask (bit 14) is cleared.
    let ctrl = cfg_read(&mut m, bdf, msix_cap + 0x02, 2) as u16;
    let ctrl = (ctrl & !(1 << 14)) | (1 << 15);
    cfg_write(&mut m, bdf, msix_cap + 0x02, 2, u32::from(ctrl));

    // BAR0 layout for Aero's virtio-pci contract.
    const COMMON: u64 = profile::VIRTIO_COMMON_CFG_BAR0_OFFSET as u64;
    const NOTIFY: u64 = profile::VIRTIO_NOTIFY_CFG_BAR0_OFFSET as u64;
    const NOTIFY_MULT: u64 = profile::VIRTIO_NOTIFY_OFF_MULTIPLIER as u64;

    // Basic feature negotiation (accept whatever the device offers).
    m.write_physical_u8(bar0_base + COMMON + 0x14, 1); // ACKNOWLEDGE
    m.write_physical_u8(bar0_base + COMMON + 0x14, 1 | 2); // + DRIVER

    m.write_physical_u32(bar0_base + COMMON, 0);
    let f0 = m.read_physical_u32(bar0_base + COMMON + 0x04);
    m.write_physical_u32(bar0_base + COMMON + 0x08, 0);
    m.write_physical_u32(bar0_base + COMMON + 0x0c, f0);

    m.write_physical_u32(bar0_base + COMMON, 1);
    let f1 = m.read_physical_u32(bar0_base + COMMON + 0x04);
    m.write_physical_u32(bar0_base + COMMON + 0x08, 1);
    m.write_physical_u32(bar0_base + COMMON + 0x0c, f1);

    m.write_physical_u8(bar0_base + COMMON + 0x14, 1 | 2 | 8); // + FEATURES_OK
    m.write_physical_u8(bar0_base + COMMON + 0x14, 1 | 2 | 8 | 4); // + DRIVER_OK

    // Configure queue 0.
    let desc = 0x200000;
    let avail = 0x201000;
    let used = 0x202000;

    m.write_physical_u16(bar0_base + COMMON + 0x16, 0); // queue_select
    assert!(m.read_physical_u16(bar0_base + COMMON + 0x18) >= 2);
    // Assign MSI-X vector 0 to queue 0.
    m.write_physical_u16(bar0_base + COMMON + 0x1a, 0);
    m.write_physical_u64(bar0_base + COMMON + 0x20, desc);
    m.write_physical_u64(bar0_base + COMMON + 0x28, avail);
    m.write_physical_u64(bar0_base + COMMON + 0x30, used);
    m.write_physical_u16(bar0_base + COMMON + 0x1c, 1); // queue_enable

    // Build a minimal FLUSH request.
    const VIRTIO_BLK_T_FLUSH: u32 = 4;
    let header = 0x203000;
    let status = 0x204000;
    m.write_physical_u32(header, VIRTIO_BLK_T_FLUSH);
    m.write_physical_u32(header + 4, 0);
    m.write_physical_u64(header + 8, 0);
    m.write_physical_u8(status, 0xff);

    write_desc(&mut m, desc, 0, header, 16, VIRTQ_DESC_F_NEXT, 1);
    write_desc(&mut m, desc, 1, status, 1, VIRTQ_DESC_F_WRITE, 0);

    m.write_physical_u16(avail, 0);
    m.write_physical_u16(avail + 2, 1);
    m.write_physical_u16(avail + 4, 0);
    m.write_physical_u16(used, 0);
    m.write_physical_u16(used + 2, 0);

    assert_eq!(interrupts.borrow().get_pending(), None);

    // Doorbell queue 0, then allow the device to process.
    let notify_off = m.read_physical_u16(bar0_base + COMMON + 0x1e);
    let notify_addr = bar0_base + NOTIFY + u64::from(notify_off) * NOTIFY_MULT;
    m.write_physical_u16(notify_addr, 0);
    m.process_virtio_blk();

    assert_eq!(m.read_physical_u16(used + 2), 1);
    assert_eq!(m.read_physical_u8(status), 0);

    // MSI-X should have delivered directly to the LAPIC; legacy INTx should not be asserted.
    assert!(
        !virtio_blk.borrow().irq_level(),
        "virtio-blk should not assert legacy INTx once MSI-X is enabled"
    );
    assert_eq!(interrupts.borrow().get_pending(), Some(vector as u8));

    // Emulate guest interrupt handling to clear the pending MSI-X delivery.
    interrupts.borrow_mut().acknowledge(vector as u8);
    interrupts.borrow_mut().eoi(vector as u8);
    assert_eq!(interrupts.borrow().get_pending(), None);

    // Disable MSI-X via PCI config space (without BAR0 MMIO). The machine should mirror the MSI-X
    // enable state into the transport during polling so the device falls back to legacy INTx.
    let ctrl = cfg_read(&mut m, bdf, msix_cap + 0x02, 2) as u16;
    cfg_write(
        &mut m,
        bdf,
        msix_cap + 0x02,
        2,
        u32::from(ctrl & !(1 << 15)),
    );

    // Re-submit the same descriptor chain.
    m.write_physical_u8(status, 0xff);
    m.write_physical_u16(avail + 2, 2); // idx
    m.write_physical_u16(avail + 6, 0); // ring[1]

    m.write_physical_u16(notify_addr, 0);
    m.process_virtio_blk();

    assert_eq!(m.read_physical_u16(used + 2), 2);
    assert_eq!(m.read_physical_u8(status), 0);
    assert!(
        virtio_blk.borrow().irq_level(),
        "virtio-blk should assert legacy INTx once MSI-X is disabled"
    );
    assert_eq!(
        interrupts.borrow().get_pending(),
        None,
        "expected no MSI-X delivery once MSI-X is disabled"
    );
}

#[test]
fn virtio_blk_msix_enable_suppresses_legacy_intx_in_poll_pci_intx_lines() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_blk: true,
        // Keep the test focused on PCI INTx polling semantics.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        enable_virtio_input: false,
        enable_uhci: false,
        enable_ahci: false,
        enable_nvme: false,
        enable_ide: false,
        ..Default::default()
    })
    .unwrap();

    let bdf = profile::VIRTIO_BLK.bdf;
    let virtio_blk = m.virtio_blk().expect("virtio-blk enabled");
    let pci_intx = m.pci_intx_router().expect("pc platform enabled");
    let interrupts = m.platform_interrupts().expect("pc platform enabled");

    // Ensure legacy INTx is enabled in the guest-visible PCI command register (bit 10 clear).
    let command = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(command & !(1 << 10)));

    // Synthesize a pending legacy INTx interrupt inside the virtio transport.
    virtio_blk.borrow_mut().signal_config_interrupt();

    // Polling should drive the virtio-blk INTx level into the platform interrupt controller.
    m.poll_pci_intx_lines();
    let gsi = pci_intx.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);
    assert!(interrupts.borrow().gsi_level(gsi));

    // Enable MSI-X in the canonical PCI config space. Polling INTx lines should mirror MSI-X state
    // into the runtime virtio transport so legacy INTx becomes suppressed even without any virtio
    // queue processing.
    let msix_cap = find_capability(&mut m, bdf, aero_devices::pci::msix::PCI_CAP_ID_MSIX)
        .expect("virtio-blk should expose MSI-X capability");
    let ctrl = cfg_read(&mut m, bdf, msix_cap + 0x02, 2) as u16;
    cfg_write(
        &mut m,
        bdf,
        msix_cap + 0x02,
        2,
        u32::from((ctrl & !(1 << 14)) | (1 << 15)),
    );

    m.poll_pci_intx_lines();
    assert!(
        !interrupts.borrow().gsi_level(gsi),
        "expected legacy INTx to be suppressed once MSI-X is enabled"
    );
    assert!(
        !virtio_blk.borrow().irq_level(),
        "expected virtio transport legacy INTx line to be deasserted once MSI-X is enabled"
    );
}

#[test]
fn snapshot_restore_preserves_virtio_blk_msix_pending_bit_and_delivers_after_unmask() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 4 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_blk: true,
        // Keep the test focused on MSI-X snapshot/restore behavior.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    let virtio_blk = m.virtio_blk().expect("virtio-blk enabled");
    let bdf = profile::VIRTIO_BLK.bdf;

    // Switch into APIC mode so MSI delivery targets the LAPIC.
    m.io_write(IMCR_SELECT_PORT, 1, u32::from(IMCR_INDEX));
    m.io_write(IMCR_DATA_PORT, 1, 0x01);
    assert_eq!(interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    // Enable PCI memory decoding + bus mastering so BAR0 is reachable and DMA works.
    let cmd = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(cmd | (1 << 1) | (1 << 2)));

    // Discover BAR0.
    let bar0_lo = cfg_read(&mut m, bdf, 0x10, 4) as u64;
    let bar0_hi = cfg_read(&mut m, bdf, 0x14, 4) as u64;
    let bar0_base = (bar0_hi << 32) | (bar0_lo & !0xFu64);
    assert_ne!(bar0_base, 0, "expected virtio-blk BAR0 to be assigned");

    // Locate MSI-X capability and validate table/PBA live in BAR0.
    let msix_cap = find_capability(&mut m, bdf, aero_devices::pci::msix::PCI_CAP_ID_MSIX)
        .expect("virtio-blk should expose MSI-X capability");
    let table = cfg_read(&mut m, bdf, msix_cap + 0x04, 4);
    let pba = cfg_read(&mut m, bdf, msix_cap + 0x08, 4);
    assert_eq!(table & 0x7, 0, "MSI-X table must live in BAR0 (BIR=0)");
    assert_eq!(pba & 0x7, 0, "MSI-X PBA must live in BAR0 (BIR=0)");

    // Program MSI-X table entry 0: dest = BSP (APIC ID 0), vector = 0x61.
    let vector = 0x61u32;
    let table_offset = u64::from(table & !0x7);
    let pba_offset = u64::from(pba & !0x7);
    let entry0 = bar0_base + table_offset;
    m.write_physical_u32(entry0, 0xfee0_0000);
    m.write_physical_u32(entry0 + 0x4, 0);
    m.write_physical_u32(entry0 + 0x8, vector);
    m.write_physical_u32(entry0 + 0xc, 0); // unmasked

    // Enable MSI-X and set Function Mask (bit 14) so interrupts are latched in the PBA.
    let ctrl = cfg_read(&mut m, bdf, msix_cap + 0x02, 2) as u16;
    cfg_write(
        &mut m,
        bdf,
        msix_cap + 0x02,
        2,
        u32::from(ctrl | (1 << 15) | (1 << 14)),
    );

    // BAR0 layout for Aero's virtio-pci contract.
    const COMMON: u64 = profile::VIRTIO_COMMON_CFG_BAR0_OFFSET as u64;
    const NOTIFY: u64 = profile::VIRTIO_NOTIFY_CFG_BAR0_OFFSET as u64;
    const NOTIFY_MULT: u64 = profile::VIRTIO_NOTIFY_OFF_MULTIPLIER as u64;

    // Basic feature negotiation (accept whatever the device offers).
    m.write_physical_u8(bar0_base + COMMON + 0x14, 1); // ACKNOWLEDGE
    m.write_physical_u8(bar0_base + COMMON + 0x14, 1 | 2); // + DRIVER

    m.write_physical_u32(bar0_base + COMMON, 0);
    let f0 = m.read_physical_u32(bar0_base + COMMON + 0x04);
    m.write_physical_u32(bar0_base + COMMON + 0x08, 0);
    m.write_physical_u32(bar0_base + COMMON + 0x0c, f0);

    m.write_physical_u32(bar0_base + COMMON, 1);
    let f1 = m.read_physical_u32(bar0_base + COMMON + 0x04);
    m.write_physical_u32(bar0_base + COMMON + 0x08, 1);
    m.write_physical_u32(bar0_base + COMMON + 0x0c, f1);

    m.write_physical_u8(bar0_base + COMMON + 0x14, 1 | 2 | 8); // + FEATURES_OK
    m.write_physical_u8(bar0_base + COMMON + 0x14, 1 | 2 | 8 | 4); // + DRIVER_OK

    // Configure queue 0.
    let desc = 0x200000;
    let avail = 0x201000;
    let used = 0x202000;

    m.write_physical_u16(bar0_base + COMMON + 0x16, 0); // queue_select
    assert!(m.read_physical_u16(bar0_base + COMMON + 0x18) >= 2);
    // Assign MSI-X vector 0 to queue 0.
    m.write_physical_u16(bar0_base + COMMON + 0x1a, 0);
    m.write_physical_u64(bar0_base + COMMON + 0x20, desc);
    m.write_physical_u64(bar0_base + COMMON + 0x28, avail);
    m.write_physical_u64(bar0_base + COMMON + 0x30, used);
    m.write_physical_u16(bar0_base + COMMON + 0x1c, 1); // queue_enable

    // Build a minimal FLUSH request.
    const VIRTIO_BLK_T_FLUSH: u32 = 4;
    let header = 0x203000;
    let status = 0x204000;
    m.write_physical_u32(header, VIRTIO_BLK_T_FLUSH);
    m.write_physical_u32(header + 4, 0);
    m.write_physical_u64(header + 8, 0);
    m.write_physical_u8(status, 0xff);

    write_desc(&mut m, desc, 0, header, 16, VIRTQ_DESC_F_NEXT, 1);
    write_desc(&mut m, desc, 1, status, 1, VIRTQ_DESC_F_WRITE, 0);

    m.write_physical_u16(avail, 0);
    m.write_physical_u16(avail + 2, 1);
    m.write_physical_u16(avail + 4, 0);
    m.write_physical_u16(used, 0);
    m.write_physical_u16(used + 2, 0);

    // Doorbell queue 0, then allow the device to process and attempt to raise an MSI-X interrupt.
    let notify_off = m.read_physical_u16(bar0_base + COMMON + 0x1e);
    let notify_addr = bar0_base + NOTIFY + u64::from(notify_off) * NOTIFY_MULT;
    m.write_physical_u16(notify_addr, 0);
    m.process_virtio_blk();

    assert_eq!(m.read_physical_u16(used + 2), 1);
    assert_eq!(m.read_physical_u8(status), 0);

    // MSI-X Function Mask should suppress delivery without falling back to INTx; the vector must be
    // latched as pending in the MSI-X PBA.
    assert!(
        !virtio_blk.borrow().irq_level(),
        "virtio-blk should not assert legacy INTx while MSI-X is enabled (even if masked)"
    );
    assert_eq!(interrupts.borrow().get_pending(), None);
    let pba_bits = m.read_physical_u64(bar0_base + pba_offset);
    assert_ne!(pba_bits & 1, 0, "expected MSI-X pending bit 0 to be set");

    // Clear the virtio interrupt cause (ISR is read-to-clear). Pending MSI-X delivery should still
    // occur once Function Mask is cleared, even without a new interrupt edge.
    let _isr = m.read_physical_u8(bar0_base + u64::from(profile::VIRTIO_ISR_CFG_BAR0_OFFSET));
    assert_ne!(
        m.read_physical_u64(bar0_base + pba_offset) & 1,
        0,
        "expected PBA pending bit to remain set after clearing the ISR"
    );

    let snapshot = m.take_snapshot_full().unwrap();

    // Mutate state after snapshot so restore is an observable rewind: clear Function Mask and allow
    // the pending MSI-X vector to be delivered/cleared.
    let ctrl = cfg_read(&mut m, bdf, msix_cap + 0x02, 2) as u16;
    cfg_write(
        &mut m,
        bdf,
        msix_cap + 0x02,
        2,
        u32::from(ctrl & !(1 << 14)),
    );
    m.process_virtio_blk();
    assert_eq!(interrupts.borrow().get_pending(), Some(vector as u8));
    interrupts.borrow_mut().acknowledge(vector as u8);
    interrupts.borrow_mut().eoi(vector as u8);
    assert_eq!(interrupts.borrow().get_pending(), None);
    let pba_bits = m.read_physical_u64(bar0_base + pba_offset);
    assert_eq!(
        pba_bits & 1,
        0,
        "expected pending bit to clear after delivery"
    );

    // Restore and ensure the pending bit and masked MSI-X state are restored.
    m.restore_snapshot_bytes(&snapshot).unwrap();
    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    assert_eq!(interrupts.borrow().mode(), PlatformInterruptMode::Apic);
    assert_eq!(interrupts.borrow().get_pending(), None);

    let bar0_lo = cfg_read(&mut m, bdf, 0x10, 4) as u64;
    let bar0_hi = cfg_read(&mut m, bdf, 0x14, 4) as u64;
    let bar0_base = (bar0_hi << 32) | (bar0_lo & !0xFu64);
    let pba_bits = m.read_physical_u64(bar0_base + pba_offset);
    assert_ne!(
        pba_bits & 1,
        0,
        "expected MSI-X pending bit to survive snapshot/restore"
    );

    // Clear Function Mask post-restore and verify the pending vector is delivered and the pending
    // bit clears.
    let ctrl = cfg_read(&mut m, bdf, msix_cap + 0x02, 2) as u16;
    cfg_write(
        &mut m,
        bdf,
        msix_cap + 0x02,
        2,
        u32::from(ctrl & !(1 << 14)),
    );
    m.process_virtio_blk();
    assert_eq!(interrupts.borrow().get_pending(), Some(vector as u8));
    let pba_bits = m.read_physical_u64(bar0_base + pba_offset);
    assert_eq!(
        pba_bits & 1,
        0,
        "expected pending bit to clear after unmask"
    );
}

#[test]
fn snapshot_restore_preserves_virtio_blk_msix_vector_mask_pending_bit_and_delivers_after_unmask() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 4 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_blk: true,
        // Keep the test focused on per-vector MSI-X mask snapshot/restore behavior.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    let bdf = profile::VIRTIO_BLK.bdf;

    // Switch into APIC mode so MSI delivery targets the LAPIC.
    m.io_write(IMCR_SELECT_PORT, 1, u32::from(IMCR_INDEX));
    m.io_write(IMCR_DATA_PORT, 1, 0x01);
    assert_eq!(interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    // Enable PCI memory decoding + bus mastering so BAR0 is reachable and DMA works.
    let cmd = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(cmd | (1 << 1) | (1 << 2)));

    // Discover BAR0.
    let bar0_lo = cfg_read(&mut m, bdf, 0x10, 4) as u64;
    let bar0_hi = cfg_read(&mut m, bdf, 0x14, 4) as u64;
    let bar0_base = (bar0_hi << 32) | (bar0_lo & !0xFu64);
    assert_ne!(bar0_base, 0, "expected virtio-blk BAR0 to be assigned");

    // Locate MSI-X capability and validate table/PBA live in BAR0.
    let msix_cap = find_capability(&mut m, bdf, aero_devices::pci::msix::PCI_CAP_ID_MSIX)
        .expect("virtio-blk should expose MSI-X capability");
    let table = cfg_read(&mut m, bdf, msix_cap + 0x04, 4);
    let pba = cfg_read(&mut m, bdf, msix_cap + 0x08, 4);
    assert_eq!(table & 0x7, 0, "MSI-X table must live in BAR0 (BIR=0)");
    assert_eq!(pba & 0x7, 0, "MSI-X PBA must live in BAR0 (BIR=0)");

    // Program table entry 0 with an xAPIC message targeting vector 0x63, but keep the entry masked
    // (vector control bit 0).
    let vector = 0x63u32;
    let table_offset = u64::from(table & !0x7);
    let pba_offset = u64::from(pba & !0x7);
    let entry0 = bar0_base + table_offset;
    m.write_physical_u32(entry0, 0xfee0_0000);
    m.write_physical_u32(entry0 + 0x4, 0);
    m.write_physical_u32(entry0 + 0x8, vector);
    m.write_physical_u32(entry0 + 0xc, 1); // masked

    // Enable MSI-X (bit 15) and ensure function mask (bit 14) is cleared.
    let ctrl = cfg_read(&mut m, bdf, msix_cap + 0x02, 2) as u16;
    let ctrl = (ctrl & !(1 << 14)) | (1 << 15);
    cfg_write(&mut m, bdf, msix_cap + 0x02, 2, u32::from(ctrl));

    // BAR0 layout for Aero's virtio-pci contract.
    const COMMON: u64 = profile::VIRTIO_COMMON_CFG_BAR0_OFFSET as u64;
    const NOTIFY: u64 = profile::VIRTIO_NOTIFY_CFG_BAR0_OFFSET as u64;
    const NOTIFY_MULT: u64 = profile::VIRTIO_NOTIFY_OFF_MULTIPLIER as u64;

    // Basic feature negotiation (accept whatever the device offers).
    m.write_physical_u8(bar0_base + COMMON + 0x14, 1); // ACKNOWLEDGE
    m.write_physical_u8(bar0_base + COMMON + 0x14, 1 | 2); // + DRIVER

    m.write_physical_u32(bar0_base + COMMON, 0);
    let f0 = m.read_physical_u32(bar0_base + COMMON + 0x04);
    m.write_physical_u32(bar0_base + COMMON + 0x08, 0);
    m.write_physical_u32(bar0_base + COMMON + 0x0c, f0);

    m.write_physical_u32(bar0_base + COMMON, 1);
    let f1 = m.read_physical_u32(bar0_base + COMMON + 0x04);
    m.write_physical_u32(bar0_base + COMMON + 0x08, 1);
    m.write_physical_u32(bar0_base + COMMON + 0x0c, f1);

    m.write_physical_u8(bar0_base + COMMON + 0x14, 1 | 2 | 8); // + FEATURES_OK
    m.write_physical_u8(bar0_base + COMMON + 0x14, 1 | 2 | 8 | 4); // + DRIVER_OK

    // Configure queue 0.
    let desc = 0x200000;
    let avail = 0x201000;
    let used = 0x202000;

    m.write_physical_u16(bar0_base + COMMON + 0x16, 0); // queue_select
    assert!(m.read_physical_u16(bar0_base + COMMON + 0x18) >= 2);
    // Assign MSI-X vector 0 to queue 0.
    m.write_physical_u16(bar0_base + COMMON + 0x1a, 0);
    m.write_physical_u64(bar0_base + COMMON + 0x20, desc);
    m.write_physical_u64(bar0_base + COMMON + 0x28, avail);
    m.write_physical_u64(bar0_base + COMMON + 0x30, used);
    m.write_physical_u16(bar0_base + COMMON + 0x1c, 1); // queue_enable

    // Build a minimal FLUSH request.
    const VIRTIO_BLK_T_FLUSH: u32 = 4;
    let header = 0x203000;
    let status = 0x204000;
    m.write_physical_u32(header, VIRTIO_BLK_T_FLUSH);
    m.write_physical_u32(header + 4, 0);
    m.write_physical_u64(header + 8, 0);
    m.write_physical_u8(status, 0xff);

    write_desc(&mut m, desc, 0, header, 16, VIRTQ_DESC_F_NEXT, 1);
    write_desc(&mut m, desc, 1, status, 1, VIRTQ_DESC_F_WRITE, 0);

    m.write_physical_u16(avail, 0);
    m.write_physical_u16(avail + 2, 1);
    m.write_physical_u16(avail + 4, 0);
    m.write_physical_u16(used, 0);
    m.write_physical_u16(used + 2, 0);

    // Doorbell queue 0, then allow the device to process and attempt to raise an MSI-X interrupt.
    let notify_off = m.read_physical_u16(bar0_base + COMMON + 0x1e);
    let notify_addr = bar0_base + NOTIFY + u64::from(notify_off) * NOTIFY_MULT;
    m.write_physical_u16(notify_addr, 0);
    m.process_virtio_blk();

    assert_eq!(m.read_physical_u16(used + 2), 1);
    assert_eq!(m.read_physical_u8(status), 0);

    // While the vector is masked, there should be no MSI-X delivery and no INTx fallback, but the
    // PBA pending bit should latch.
    let virtio_blk = m.virtio_blk().expect("virtio-blk enabled");
    assert!(
        !virtio_blk.borrow().irq_level(),
        "virtio-blk should not assert legacy INTx while MSI-X is enabled (even if the entry is masked)"
    );
    assert_eq!(
        interrupts.borrow().get_pending(),
        None,
        "expected no MSI-X delivery while the vector is masked"
    );
    let pba_bits = m.read_physical_u64(bar0_base + pba_offset);
    assert_ne!(
        pba_bits & 1,
        0,
        "expected MSI-X pending bit 0 to be set while the entry is masked"
    );

    // Clear the virtio interrupt cause (ISR is read-to-clear). Pending MSI-X delivery should still
    // occur once the entry becomes unmasked, even without a new interrupt edge.
    let _isr = m.read_physical_u8(bar0_base + u64::from(profile::VIRTIO_ISR_CFG_BAR0_OFFSET));
    assert_ne!(
        m.read_physical_u64(bar0_base + pba_offset) & 1,
        0,
        "expected PBA pending bit to remain set after clearing the ISR"
    );

    let snapshot = m.take_snapshot_full().unwrap();

    // Mutate state after snapshot: unmask the vector and deliver the pending MSI-X interrupt,
    // clearing the pending bit.
    m.write_physical_u32(entry0 + 0xc, 0);
    assert_eq!(interrupts.borrow().get_pending(), Some(vector as u8));
    interrupts.borrow_mut().acknowledge(vector as u8);
    interrupts.borrow_mut().eoi(vector as u8);
    assert_eq!(interrupts.borrow().get_pending(), None);
    assert_eq!(
        m.read_physical_u64(bar0_base + pba_offset) & 1,
        0,
        "expected MSI-X pending bit 0 to clear after unmask + delivery"
    );

    m.restore_snapshot_bytes(&snapshot).unwrap();

    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    assert_eq!(interrupts.borrow().mode(), PlatformInterruptMode::Apic);
    assert_eq!(interrupts.borrow().get_pending(), None);

    // Re-read BAR0 base (may be restored/reassigned by snapshot).
    let bar0_lo = cfg_read(&mut m, bdf, 0x10, 4) as u64;
    let bar0_hi = cfg_read(&mut m, bdf, 0x14, 4) as u64;
    let bar0_base = (bar0_hi << 32) | (bar0_lo & !0xFu64);
    assert_ne!(bar0_base, 0);
    let entry0 = bar0_base + table_offset;

    // Ensure the MSI-X entry mask and pending bit were restored.
    assert_eq!(
        m.read_physical_u32(entry0 + 0xc) & 1,
        1,
        "expected MSI-X vector control mask bit to be restored"
    );
    assert_ne!(
        m.read_physical_u64(bar0_base + pba_offset) & 1,
        0,
        "expected MSI-X pending bit 0 to survive snapshot/restore"
    );

    // Unmask post-restore and verify the pending vector is delivered and the pending bit clears.
    m.write_physical_u32(entry0 + 0xc, 0);
    assert_eq!(interrupts.borrow().get_pending(), Some(vector as u8));
    interrupts.borrow_mut().acknowledge(vector as u8);
    interrupts.borrow_mut().eoi(vector as u8);
    assert_eq!(interrupts.borrow().get_pending(), None);
    assert_eq!(
        m.read_physical_u64(bar0_base + pba_offset) & 1,
        0,
        "expected MSI-X pending bit 0 to clear after restore + unmask + delivery"
    );
}

#[test]
fn virtio_blk_msix_vector_mask_defers_delivery_until_unmasked() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 4 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_blk: true,
        // Keep the test focused on per-vector MSI-X mask semantics.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    let virtio_blk = m.virtio_blk().expect("virtio-blk enabled");
    let bdf = profile::VIRTIO_BLK.bdf;

    // Switch into APIC mode so MSI delivery targets the LAPIC.
    m.io_write(IMCR_SELECT_PORT, 1, u32::from(IMCR_INDEX));
    m.io_write(IMCR_DATA_PORT, 1, 0x01);
    assert_eq!(interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    // Enable PCI memory decoding + bus mastering so BAR0 is reachable and DMA works.
    let cmd = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(cmd | (1 << 1) | (1 << 2)));

    // Discover BAR0.
    let bar0_lo = cfg_read(&mut m, bdf, 0x10, 4) as u64;
    let bar0_hi = cfg_read(&mut m, bdf, 0x14, 4) as u64;
    let bar0_base = (bar0_hi << 32) | (bar0_lo & !0xFu64);
    assert_ne!(bar0_base, 0, "expected virtio-blk BAR0 to be assigned");

    // Locate MSI-X capability and validate table/PBA live in BAR0.
    let msix_cap = find_capability(&mut m, bdf, aero_devices::pci::msix::PCI_CAP_ID_MSIX)
        .expect("virtio-blk should expose MSI-X capability");
    let table = cfg_read(&mut m, bdf, msix_cap + 0x04, 4);
    let pba = cfg_read(&mut m, bdf, msix_cap + 0x08, 4);
    assert_eq!(table & 0x7, 0, "MSI-X table must live in BAR0 (BIR=0)");
    assert_eq!(pba & 0x7, 0, "MSI-X PBA must live in BAR0 (BIR=0)");

    // Program table entry 0 with an xAPIC message targeting vector 0x62, but keep the entry masked
    // (vector control bit 0).
    let vector = 0x62u32;
    let table_offset = u64::from(table & !0x7);
    let pba_offset = u64::from(pba & !0x7);
    let entry0 = bar0_base + table_offset;
    m.write_physical_u32(entry0, 0xfee0_0000);
    m.write_physical_u32(entry0 + 0x4, 0);
    m.write_physical_u32(entry0 + 0x8, vector);
    m.write_physical_u32(entry0 + 0xc, 1); // masked

    // Enable MSI-X (bit 15) and ensure function mask (bit 14) is cleared.
    let ctrl = cfg_read(&mut m, bdf, msix_cap + 0x02, 2) as u16;
    let ctrl = (ctrl & !(1 << 14)) | (1 << 15);
    cfg_write(&mut m, bdf, msix_cap + 0x02, 2, u32::from(ctrl));

    // BAR0 layout for Aero's virtio-pci contract.
    const COMMON: u64 = profile::VIRTIO_COMMON_CFG_BAR0_OFFSET as u64;
    const NOTIFY: u64 = profile::VIRTIO_NOTIFY_CFG_BAR0_OFFSET as u64;
    const NOTIFY_MULT: u64 = profile::VIRTIO_NOTIFY_OFF_MULTIPLIER as u64;

    // Basic feature negotiation (accept whatever the device offers).
    m.write_physical_u8(bar0_base + COMMON + 0x14, 1); // ACKNOWLEDGE
    m.write_physical_u8(bar0_base + COMMON + 0x14, 1 | 2); // + DRIVER

    m.write_physical_u32(bar0_base + COMMON, 0);
    let f0 = m.read_physical_u32(bar0_base + COMMON + 0x04);
    m.write_physical_u32(bar0_base + COMMON + 0x08, 0);
    m.write_physical_u32(bar0_base + COMMON + 0x0c, f0);

    m.write_physical_u32(bar0_base + COMMON, 1);
    let f1 = m.read_physical_u32(bar0_base + COMMON + 0x04);
    m.write_physical_u32(bar0_base + COMMON + 0x08, 1);
    m.write_physical_u32(bar0_base + COMMON + 0x0c, f1);

    m.write_physical_u8(bar0_base + COMMON + 0x14, 1 | 2 | 8); // + FEATURES_OK
    m.write_physical_u8(bar0_base + COMMON + 0x14, 1 | 2 | 8 | 4); // + DRIVER_OK

    // Configure queue 0.
    let desc = 0x200000;
    let avail = 0x201000;
    let used = 0x202000;

    m.write_physical_u16(bar0_base + COMMON + 0x16, 0); // queue_select
    assert!(m.read_physical_u16(bar0_base + COMMON + 0x18) >= 2);
    // Assign MSI-X vector 0 to queue 0.
    m.write_physical_u16(bar0_base + COMMON + 0x1a, 0);
    m.write_physical_u64(bar0_base + COMMON + 0x20, desc);
    m.write_physical_u64(bar0_base + COMMON + 0x28, avail);
    m.write_physical_u64(bar0_base + COMMON + 0x30, used);
    m.write_physical_u16(bar0_base + COMMON + 0x1c, 1); // queue_enable

    // Build a minimal FLUSH request.
    const VIRTIO_BLK_T_FLUSH: u32 = 4;
    let header = 0x203000;
    let status = 0x204000;
    m.write_physical_u32(header, VIRTIO_BLK_T_FLUSH);
    m.write_physical_u32(header + 4, 0);
    m.write_physical_u64(header + 8, 0);
    m.write_physical_u8(status, 0xff);

    write_desc(&mut m, desc, 0, header, 16, VIRTQ_DESC_F_NEXT, 1);
    write_desc(&mut m, desc, 1, status, 1, VIRTQ_DESC_F_WRITE, 0);

    m.write_physical_u16(avail, 0);
    m.write_physical_u16(avail + 2, 1);
    m.write_physical_u16(avail + 4, 0);
    m.write_physical_u16(used, 0);
    m.write_physical_u16(used + 2, 0);

    assert_eq!(interrupts.borrow().get_pending(), None);

    // Doorbell queue 0, then allow the device to process.
    let notify_off = m.read_physical_u16(bar0_base + COMMON + 0x1e);
    let notify_addr = bar0_base + NOTIFY + u64::from(notify_off) * NOTIFY_MULT;
    m.write_physical_u16(notify_addr, 0);
    m.process_virtio_blk();

    assert_eq!(m.read_physical_u16(used + 2), 1);
    assert_eq!(m.read_physical_u8(status), 0);

    // While the vector is masked, there should be no MSI-X delivery and no INTx fallback, but the
    // PBA pending bit should latch.
    assert!(
        !virtio_blk.borrow().irq_level(),
        "virtio-blk should not assert legacy INTx while MSI-X is enabled (even if the entry is masked)"
    );
    assert_eq!(
        interrupts.borrow().get_pending(),
        None,
        "expected no MSI-X delivery while the vector is masked"
    );
    let pba_bits = m.read_physical_u64(bar0_base + pba_offset);
    assert_ne!(
        pba_bits & 1,
        0,
        "expected MSI-X pending bit 0 to be set while the entry is masked"
    );

    // Clear the virtio interrupt cause (ISR is read-to-clear). Pending MSI-X delivery should still
    // occur once the entry becomes unmasked.
    let _isr = m.read_physical_u8(bar0_base + u64::from(profile::VIRTIO_ISR_CFG_BAR0_OFFSET));

    // Unmask the vector via table write; the pending interrupt should be delivered immediately.
    m.write_physical_u32(entry0 + 0xc, 0);
    assert_eq!(interrupts.borrow().get_pending(), Some(vector as u8));
    interrupts.borrow_mut().acknowledge(vector as u8);
    interrupts.borrow_mut().eoi(vector as u8);
    assert_eq!(interrupts.borrow().get_pending(), None);
    let pba_bits = m.read_physical_u64(bar0_base + pba_offset);
    assert_eq!(
        pba_bits & 1,
        0,
        "expected MSI-X pending bit 0 to clear after unmask + delivery"
    );
}
