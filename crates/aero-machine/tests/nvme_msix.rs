#![cfg(not(target_arch = "wasm32"))]

use aero_devices::a20_gate::A20_GATE_PORT;
use aero_devices::pci::{profile, PciBdf, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_machine::{Machine, MachineConfig};
use aero_platform::interrupts::{
    InterruptController as PlatformInterruptController, PlatformInterruptMode,
};
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

#[test]
fn nvme_msix_delivers_to_lapic_in_apic_mode() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_nvme: true,
        // Keep the test focused on PCI + NVMe.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    // Ensure high MMIO addresses decode correctly (avoid A20 aliasing).
    m.io_write(A20_GATE_PORT, 1, 0x02);

    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    interrupts
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);
    assert_eq!(interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    let bdf = profile::NVME_CONTROLLER.bdf;

    // Enable PCI memory decoding + bus mastering (required for MMIO + DMA).
    let cmd = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(cmd | (1 << 1) | (1 << 2)));

    // Read BAR0 base (64-bit MMIO BAR).
    let bar0_lo = cfg_read(&mut m, bdf, 0x10, 4) as u64;
    let bar0_hi = cfg_read(&mut m, bdf, 0x14, 4) as u64;
    let bar0_base = (bar0_hi << 32) | (bar0_lo & !0xFu64);
    assert_ne!(
        bar0_base, 0,
        "expected NVMe BAR0 to be assigned during BIOS POST"
    );

    // Enable MSI-X (capability control bit 15).
    let msix_cap = find_capability(&mut m, bdf, aero_devices::pci::msix::PCI_CAP_ID_MSIX)
        .expect("NVMe should expose MSI-X capability");
    let ctrl = cfg_read(&mut m, bdf, msix_cap + 0x02, 2) as u16;
    cfg_write(&mut m, bdf, msix_cap + 0x02, 2, u32::from(ctrl | (1 << 15)));

    // Program MSI-X table entry 0 via guest physical MMIO.
    let table = cfg_read(&mut m, bdf, msix_cap + 0x04, 4);
    assert_eq!(table & 0x7, 0, "MSI-X table must live in BAR0 (BIR=0)");
    let table_offset = u64::from(table & !0x7);

    let vector: u8 = 0x67;
    let entry0 = bar0_base + table_offset;
    m.write_physical_u32(entry0, 0xfee0_0000);
    m.write_physical_u32(entry0 + 0x4, 0);
    m.write_physical_u32(entry0 + 0x8, u32::from(vector));
    m.write_physical_u32(entry0 + 0xc, 0); // unmasked

    // Issue admin IDENTIFY via BAR0 MMIO.
    let asq = 0x10000u64;
    let acq = 0x20000u64;
    let id_buf = 0x30000u64;

    m.write_physical_u32(bar0_base + 0x0024, 0x000f_000f); // AQA
    m.write_physical_u64(bar0_base + 0x0028, asq); // ASQ
    m.write_physical_u64(bar0_base + 0x0030, acq); // ACQ
    m.write_physical_u32(bar0_base + 0x0014, 1); // CC.EN

    let mut cmd = [0u8; 64];
    cmd[0] = 0x06; // IDENTIFY
    cmd[2..4].copy_from_slice(&0x1234u16.to_le_bytes()); // CID
    cmd[24..32].copy_from_slice(&id_buf.to_le_bytes()); // PRP1
    cmd[40..44].copy_from_slice(&0x01u32.to_le_bytes()); // CDW10: CNS=1 (controller)
    m.write_physical(asq, &cmd);

    // Ring SQ0 tail doorbell.
    m.write_physical_u32(bar0_base + 0x1000, 1);

    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );
    m.process_nvme();
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        Some(vector)
    );
}

#[test]
fn nvme_msix_function_mask_defers_delivery_until_unmasked() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_nvme: true,
        // Keep the test focused on PCI + NVMe.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    // Ensure high MMIO addresses decode correctly (avoid A20 aliasing).
    m.io_write(A20_GATE_PORT, 1, 0x02);

    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    interrupts
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);
    assert_eq!(interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    let nvme = m.nvme().expect("nvme enabled");
    let bdf = profile::NVME_CONTROLLER.bdf;

    // Enable PCI memory decoding + bus mastering (required for MMIO + DMA).
    let cmd = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(cmd | (1 << 1) | (1 << 2)));

    // Read BAR0 base (64-bit MMIO BAR).
    let bar0_lo = cfg_read(&mut m, bdf, 0x10, 4) as u64;
    let bar0_hi = cfg_read(&mut m, bdf, 0x14, 4) as u64;
    let bar0_base = (bar0_hi << 32) | (bar0_lo & !0xFu64);
    assert_ne!(
        bar0_base, 0,
        "expected NVMe BAR0 to be assigned during BIOS POST"
    );

    // Locate MSI-X capability and validate table/PBA live in BAR0.
    let msix_cap = find_capability(&mut m, bdf, aero_devices::pci::msix::PCI_CAP_ID_MSIX)
        .expect("NVMe should expose MSI-X capability");
    let table = cfg_read(&mut m, bdf, msix_cap + 0x04, 4);
    let pba = cfg_read(&mut m, bdf, msix_cap + 0x08, 4);
    assert_eq!(table & 0x7, 0, "MSI-X table must live in BAR0 (BIR=0)");
    assert_eq!(pba & 0x7, 0, "MSI-X PBA must live in BAR0 (BIR=0)");
    let table_offset = u64::from(table & !0x7);
    let pba_offset = u64::from(pba & !0x7);

    // Program table entry 0 via guest physical MMIO.
    let vector: u8 = 0x68;
    let entry0 = bar0_base + table_offset;
    m.write_physical_u32(entry0, 0xfee0_0000);
    m.write_physical_u32(entry0 + 0x4, 0);
    m.write_physical_u32(entry0 + 0x8, u32::from(vector));
    m.write_physical_u32(entry0 + 0xc, 0); // unmasked

    // Enable MSI-X and set the function mask bit.
    let ctrl = cfg_read(&mut m, bdf, msix_cap + 0x02, 2) as u16;
    cfg_write(
        &mut m,
        bdf,
        msix_cap + 0x02,
        2,
        u32::from(ctrl | (1 << 15) | (1 << 14)),
    );

    // Issue admin IDENTIFY via BAR0 MMIO.
    let asq = 0x10000u64;
    let acq = 0x20000u64;
    let id_buf = 0x30000u64;

    m.write_physical_u32(bar0_base + 0x0024, 0x000f_000f); // AQA
    m.write_physical_u64(bar0_base + 0x0028, asq); // ASQ
    m.write_physical_u64(bar0_base + 0x0030, acq); // ACQ
    m.write_physical_u32(bar0_base + 0x0014, 1); // CC.EN

    let mut cmd = [0u8; 64];
    cmd[0] = 0x06; // IDENTIFY
    cmd[2..4].copy_from_slice(&0x1234u16.to_le_bytes()); // CID
    cmd[24..32].copy_from_slice(&id_buf.to_le_bytes()); // PRP1
    cmd[40..44].copy_from_slice(&0x01u32.to_le_bytes()); // CDW10: CNS=1 (controller)
    m.write_physical(asq, &cmd);

    // Ring SQ0 tail doorbell.
    m.write_physical_u32(bar0_base + 0x1000, 1);

    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );
    m.process_nvme();

    // MSI-X should not deliver while function-masked. It also must not fall back to legacy INTx.
    assert!(
        !nvme.borrow().irq_level(),
        "NVMe should not assert legacy INTx while MSI-X is enabled (even if masked)"
    );
    assert!(
        nvme.borrow().irq_pending(),
        "expected NVMe to have an interrupt pending (completion posted)"
    );
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None,
        "expected no MSI-X delivery while MSI-X is function-masked"
    );

    let pba_bits = m.read_physical_u64(bar0_base + pba_offset);
    assert_ne!(
        pba_bits & 1,
        0,
        "expected MSI-X pending bit 0 to be set while function-masked"
    );

    // Clear function mask and allow the device to re-drive pending MSI-X vectors.
    let ctrl = cfg_read(&mut m, bdf, msix_cap + 0x02, 2) as u16;
    cfg_write(
        &mut m,
        bdf,
        msix_cap + 0x02,
        2,
        u32::from(ctrl & !(1 << 14)),
    );

    m.process_nvme();
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        Some(vector)
    );
    let pba_bits = m.read_physical_u64(bar0_base + pba_offset);
    assert_eq!(
        pba_bits & 1,
        0,
        "expected MSI-X pending bit 0 to be cleared after unmask + delivery"
    );
}

#[test]
fn nvme_msix_function_mask_delivers_pending_after_unmask_even_when_interrupt_cleared() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_nvme: true,
        // Keep the test focused on MSI-X pending-bit semantics when the underlying NVMe interrupt
        // condition is cleared before unmasking.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    // Ensure high MMIO addresses decode correctly (avoid A20 aliasing).
    m.io_write(A20_GATE_PORT, 1, 0x02);

    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    interrupts
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);
    assert_eq!(interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    let nvme = m.nvme().expect("nvme enabled");
    let bdf = profile::NVME_CONTROLLER.bdf;

    // Enable PCI memory decoding + bus mastering (required for MMIO + DMA).
    let cmd = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(cmd | (1 << 1) | (1 << 2)));

    // Read BAR0 base (64-bit MMIO BAR).
    let bar0_lo = cfg_read(&mut m, bdf, 0x10, 4) as u64;
    let bar0_hi = cfg_read(&mut m, bdf, 0x14, 4) as u64;
    let bar0_base = (bar0_hi << 32) | (bar0_lo & !0xFu64);
    assert_ne!(
        bar0_base, 0,
        "expected NVMe BAR0 to be assigned during BIOS POST"
    );

    // Locate MSI-X capability and validate table/PBA live in BAR0.
    let msix_cap = find_capability(&mut m, bdf, aero_devices::pci::msix::PCI_CAP_ID_MSIX)
        .expect("NVMe should expose MSI-X capability");
    let table = cfg_read(&mut m, bdf, msix_cap + 0x04, 4);
    let pba = cfg_read(&mut m, bdf, msix_cap + 0x08, 4);
    assert_eq!(table & 0x7, 0, "MSI-X table must live in BAR0 (BIR=0)");
    assert_eq!(pba & 0x7, 0, "MSI-X PBA must live in BAR0 (BIR=0)");
    let table_offset = u64::from(table & !0x7);
    let pba_offset = u64::from(pba & !0x7);

    // Program table entry 0 via guest physical MMIO.
    let vector: u8 = 0x6d;
    let entry0 = bar0_base + table_offset;
    m.write_physical_u32(entry0, 0xfee0_0000);
    m.write_physical_u32(entry0 + 0x4, 0);
    m.write_physical_u32(entry0 + 0x8, u32::from(vector));
    m.write_physical_u32(entry0 + 0xc, 0); // unmasked

    // Enable MSI-X and set the function mask bit.
    let ctrl = cfg_read(&mut m, bdf, msix_cap + 0x02, 2) as u16;
    cfg_write(
        &mut m,
        bdf,
        msix_cap + 0x02,
        2,
        u32::from(ctrl | (1 << 15) | (1 << 14)),
    );

    // Issue admin IDENTIFY via BAR0 MMIO.
    let asq = 0x10000u64;
    let acq = 0x20000u64;
    let id_buf = 0x30000u64;

    m.write_physical_u32(bar0_base + 0x0024, 0x000f_000f); // AQA
    m.write_physical_u64(bar0_base + 0x0028, asq); // ASQ
    m.write_physical_u64(bar0_base + 0x0030, acq); // ACQ
    m.write_physical_u32(bar0_base + 0x0014, 1); // CC.EN

    let mut cmd = [0u8; 64];
    cmd[0] = 0x06; // IDENTIFY
    cmd[2..4].copy_from_slice(&0x1234u16.to_le_bytes()); // CID
    cmd[24..32].copy_from_slice(&id_buf.to_le_bytes()); // PRP1
    cmd[40..44].copy_from_slice(&0x01u32.to_le_bytes()); // CDW10: CNS=1 (controller)
    m.write_physical(asq, &cmd);

    // Ring SQ0 tail doorbell.
    m.write_physical_u32(bar0_base + 0x1000, 1);

    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );
    m.process_nvme();

    // MSI-X should not deliver while function-masked. It also must not fall back to legacy INTx.
    assert!(
        !nvme.borrow().irq_level(),
        "NVMe should not assert legacy INTx while MSI-X is enabled (even if masked)"
    );
    assert!(
        nvme.borrow().irq_pending(),
        "expected NVMe to have an interrupt condition pending (completion posted)"
    );
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None,
        "expected no MSI-X delivery while MSI-X is function-masked"
    );
    assert_ne!(
        m.read_physical_u64(bar0_base + pba_offset) & 1,
        0,
        "expected MSI-X pending bit 0 to be set while function-masked"
    );

    // Clear the underlying NVMe interrupt condition by consuming the completion queue entry.
    //
    // Admin CQ0 head doorbell lives at BAR0 + 0x1004 (DSTRD=0, QID=0).
    m.write_physical_u32(bar0_base + 0x1004, 1);
    assert!(
        !nvme.borrow().irq_pending(),
        "expected NVMe interrupt condition to clear after updating CQ head"
    );
    assert_ne!(
        m.read_physical_u64(bar0_base + pba_offset) & 1,
        0,
        "expected MSI-X pending bit to remain set after clearing the interrupt condition"
    );

    // Clear function mask and allow the device to re-drive pending MSI-X vectors. Pending delivery
    // must occur even though the original NVMe interrupt condition has been cleared.
    let ctrl = cfg_read(&mut m, bdf, msix_cap + 0x02, 2) as u16;
    cfg_write(
        &mut m,
        bdf,
        msix_cap + 0x02,
        2,
        u32::from(ctrl & !(1 << 14)),
    );
    m.process_nvme();
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        Some(vector)
    );
    let pba_bits = m.read_physical_u64(bar0_base + pba_offset);
    assert_eq!(
        pba_bits & 1,
        0,
        "expected MSI-X pending bit 0 to be cleared after unmask + delivery"
    );
}

#[test]
fn snapshot_restore_preserves_nvme_msix_pending_bit_and_delivers_after_unmask() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_nvme: true,
        // Keep the test focused on NVMe + snapshot + MSI-X pending-bit behavior.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    // Ensure high MMIO addresses decode correctly (avoid A20 aliasing).
    m.io_write(A20_GATE_PORT, 1, 0x02);

    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    interrupts
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);
    assert_eq!(interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    let nvme = m.nvme().expect("nvme enabled");
    let bdf = profile::NVME_CONTROLLER.bdf;

    // Enable PCI memory decoding + bus mastering (required for MMIO + DMA).
    let cmd = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(cmd | (1 << 1) | (1 << 2)));

    // Read BAR0 base (64-bit MMIO BAR).
    let bar0_lo = cfg_read(&mut m, bdf, 0x10, 4) as u64;
    let bar0_hi = cfg_read(&mut m, bdf, 0x14, 4) as u64;
    let bar0_base = (bar0_hi << 32) | (bar0_lo & !0xFu64);
    assert_ne!(
        bar0_base, 0,
        "expected NVMe BAR0 to be assigned during BIOS POST"
    );

    // Locate MSI-X capability and validate table/PBA live in BAR0.
    let msix_cap = find_capability(&mut m, bdf, aero_devices::pci::msix::PCI_CAP_ID_MSIX)
        .expect("NVMe should expose MSI-X capability");
    let table = cfg_read(&mut m, bdf, msix_cap + 0x04, 4);
    let pba = cfg_read(&mut m, bdf, msix_cap + 0x08, 4);
    assert_eq!(table & 0x7, 0, "MSI-X table must live in BAR0 (BIR=0)");
    assert_eq!(pba & 0x7, 0, "MSI-X PBA must live in BAR0 (BIR=0)");
    let table_offset = u64::from(table & !0x7);
    let pba_offset = u64::from(pba & !0x7);

    // Program MSI-X table entry 0 via guest physical MMIO.
    let vector: u8 = 0x69;
    let entry0 = bar0_base + table_offset;
    m.write_physical_u32(entry0, 0xfee0_0000);
    m.write_physical_u32(entry0 + 0x4, 0);
    m.write_physical_u32(entry0 + 0x8, u32::from(vector));
    m.write_physical_u32(entry0 + 0xc, 0); // unmasked

    // Enable MSI-X and set the function mask bit.
    let ctrl = cfg_read(&mut m, bdf, msix_cap + 0x02, 2) as u16;
    cfg_write(
        &mut m,
        bdf,
        msix_cap + 0x02,
        2,
        u32::from(ctrl | (1 << 15) | (1 << 14)),
    );

    // Issue admin IDENTIFY via BAR0 MMIO.
    let asq = 0x10000u64;
    let acq = 0x20000u64;
    let id_buf = 0x30000u64;

    m.write_physical_u32(bar0_base + 0x0024, 0x000f_000f); // AQA
    m.write_physical_u64(bar0_base + 0x0028, asq); // ASQ
    m.write_physical_u64(bar0_base + 0x0030, acq); // ACQ
    m.write_physical_u32(bar0_base + 0x0014, 1); // CC.EN

    let mut cmd = [0u8; 64];
    cmd[0] = 0x06; // IDENTIFY
    cmd[2..4].copy_from_slice(&0x1234u16.to_le_bytes()); // CID
    cmd[24..32].copy_from_slice(&id_buf.to_le_bytes()); // PRP1
    cmd[40..44].copy_from_slice(&0x01u32.to_le_bytes()); // CDW10: CNS=1 (controller)
    m.write_physical(asq, &cmd);

    // Ring SQ0 tail doorbell.
    m.write_physical_u32(bar0_base + 0x1000, 1);

    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );
    m.process_nvme();

    assert!(
        nvme.borrow().irq_pending(),
        "expected NVMe to have an interrupt pending (completion posted)"
    );
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None,
        "expected no MSI-X delivery while MSI-X is function-masked"
    );
    let pba_bits = m.read_physical_u64(bar0_base + pba_offset);
    assert_ne!(
        pba_bits & 1,
        0,
        "expected MSI-X pending bit 0 to be set while function-masked"
    );

    // Clear the NVMe interrupt condition before snapshotting by consuming the completion entry
    // (advance CQ0 head). Pending MSI-X delivery must still occur later due to the PBA pending bit.
    m.write_physical_u32(bar0_base + 0x1004, 1); // CQ0 head = 1
    assert!(
        !nvme.borrow().irq_pending(),
        "expected NVMe interrupt condition to be cleared after consuming CQ0 completion"
    );
    assert_ne!(
        m.read_physical_u64(bar0_base + pba_offset) & 1,
        0,
        "expected MSI-X pending bit 0 to remain set after clearing interrupt condition"
    );

    let snapshot = m.take_snapshot_full().unwrap();

    // Mutate state after snapshot: clear function mask and deliver the pending MSI-X vector.
    let ctrl = cfg_read(&mut m, bdf, msix_cap + 0x02, 2) as u16;
    cfg_write(
        &mut m,
        bdf,
        msix_cap + 0x02,
        2,
        u32::from(ctrl & !(1 << 14)),
    );
    m.process_nvme();
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        Some(vector)
    );
    interrupts.borrow_mut().acknowledge(vector);
    interrupts.borrow_mut().eoi(vector);
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );
    let pba_bits = m.read_physical_u64(bar0_base + pba_offset);
    assert_eq!(
        pba_bits & 1,
        0,
        "expected pending bit to clear after delivery"
    );

    m.restore_snapshot_bytes(&snapshot).unwrap();

    // Ensure high MMIO addresses decode correctly post-restore as well.
    m.io_write(A20_GATE_PORT, 1, 0x02);

    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    assert_eq!(interrupts.borrow().mode(), PlatformInterruptMode::Apic);
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );

    // Ensure MSI-X enable + function mask bits were restored in the canonical PCI config space.
    let ctrl_restored = cfg_read(&mut m, bdf, msix_cap + 0x02, 2) as u16;
    assert_ne!(
        ctrl_restored & (1 << 15),
        0,
        "expected MSI-X enable bit restored"
    );
    assert_ne!(
        ctrl_restored & (1 << 14),
        0,
        "expected MSI-X function mask bit restored"
    );

    // Ensure MSI-X PBA pending bit was restored.
    let bar0_lo = cfg_read(&mut m, bdf, 0x10, 4) as u64;
    let bar0_hi = cfg_read(&mut m, bdf, 0x14, 4) as u64;
    let bar0_base = (bar0_hi << 32) | (bar0_lo & !0xFu64);
    let pba_bits = m.read_physical_u64(bar0_base + pba_offset);
    assert_ne!(
        pba_bits & 1,
        0,
        "expected MSI-X pending bit 0 to survive snapshot/restore"
    );
    let nvme = m.nvme().expect("nvme enabled");
    assert!(
        !nvme.borrow().irq_pending(),
        "expected NVMe interrupt condition to remain cleared across snapshot/restore"
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
    m.process_nvme();
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        Some(vector)
    );
    let pba_bits = m.read_physical_u64(bar0_base + pba_offset);
    assert_eq!(
        pba_bits & 1,
        0,
        "expected pending bit to clear after unmask"
    );
}

#[test]
fn nvme_msix_vector_mask_defers_delivery_until_unmasked() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_nvme: true,
        // Keep the test focused on per-vector MSI-X mask semantics.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    // Ensure high MMIO addresses decode correctly (avoid A20 aliasing).
    m.io_write(A20_GATE_PORT, 1, 0x02);

    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    interrupts
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);
    assert_eq!(interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    let nvme = m.nvme().expect("nvme enabled");
    let bdf = profile::NVME_CONTROLLER.bdf;

    // Enable PCI memory decoding + bus mastering (required for MMIO + DMA).
    let cmd = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(cmd | (1 << 1) | (1 << 2)));

    // Read BAR0 base (64-bit MMIO BAR).
    let bar0_lo = cfg_read(&mut m, bdf, 0x10, 4) as u64;
    let bar0_hi = cfg_read(&mut m, bdf, 0x14, 4) as u64;
    let bar0_base = (bar0_hi << 32) | (bar0_lo & !0xFu64);
    assert_ne!(
        bar0_base, 0,
        "expected NVMe BAR0 to be assigned during BIOS POST"
    );

    // Locate MSI-X capability and validate table/PBA live in BAR0.
    let msix_cap = find_capability(&mut m, bdf, aero_devices::pci::msix::PCI_CAP_ID_MSIX)
        .expect("NVMe should expose MSI-X capability");
    let table = cfg_read(&mut m, bdf, msix_cap + 0x04, 4);
    let pba = cfg_read(&mut m, bdf, msix_cap + 0x08, 4);
    assert_eq!(table & 0x7, 0, "MSI-X table must live in BAR0 (BIR=0)");
    assert_eq!(pba & 0x7, 0, "MSI-X PBA must live in BAR0 (BIR=0)");
    let table_offset = u64::from(table & !0x7);
    let pba_offset = u64::from(pba & !0x7);

    // Program MSI-X table entry 0: vector = 0x6a, but keep the entry masked (vector control bit 0).
    let vector: u8 = 0x6a;
    let entry0 = bar0_base + table_offset;
    m.write_physical_u32(entry0, 0xfee0_0000);
    m.write_physical_u32(entry0 + 0x4, 0);
    m.write_physical_u32(entry0 + 0x8, u32::from(vector));
    m.write_physical_u32(entry0 + 0xc, 1); // masked

    // Enable MSI-X (bit 15) and ensure function mask (bit 14) is cleared.
    let ctrl = cfg_read(&mut m, bdf, msix_cap + 0x02, 2) as u16;
    cfg_write(
        &mut m,
        bdf,
        msix_cap + 0x02,
        2,
        u32::from((ctrl & !(1 << 14)) | (1 << 15)),
    );

    // Issue admin IDENTIFY via BAR0 MMIO.
    let asq = 0x10000u64;
    let acq = 0x20000u64;
    let id_buf = 0x30000u64;

    m.write_physical_u32(bar0_base + 0x0024, 0x000f_000f); // AQA
    m.write_physical_u64(bar0_base + 0x0028, asq); // ASQ
    m.write_physical_u64(bar0_base + 0x0030, acq); // ACQ
    m.write_physical_u32(bar0_base + 0x0014, 1); // CC.EN

    let mut cmd = [0u8; 64];
    cmd[0] = 0x06; // IDENTIFY
    cmd[2..4].copy_from_slice(&0x1234u16.to_le_bytes()); // CID
    cmd[24..32].copy_from_slice(&id_buf.to_le_bytes()); // PRP1
    cmd[40..44].copy_from_slice(&0x01u32.to_le_bytes()); // CDW10: CNS=1 (controller)
    m.write_physical(asq, &cmd);

    // Ring SQ0 tail doorbell.
    m.write_physical_u32(bar0_base + 0x1000, 1);

    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );
    m.process_nvme();

    // While the entry is masked, there should be no MSI-X delivery and no INTx fallback, but the
    // PBA pending bit should latch.
    assert!(
        !nvme.borrow().irq_level(),
        "NVMe should not assert legacy INTx while MSI-X is enabled (even if the entry is masked)"
    );
    assert!(
        nvme.borrow().irq_pending(),
        "expected NVMe to have an interrupt pending (completion posted)"
    );
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None,
        "expected no MSI-X delivery while the entry is masked"
    );
    let pba_bits = m.read_physical_u64(bar0_base + pba_offset);
    assert_ne!(
        pba_bits & 1,
        0,
        "expected MSI-X pending bit 0 to be set while the entry is masked"
    );

    // Unmask the vector via table write. This should deliver the pending interrupt immediately
    // (without requiring additional NVMe controller work).
    m.write_physical_u32(entry0 + 0xc, 0);
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        Some(vector)
    );
    interrupts.borrow_mut().acknowledge(vector);
    interrupts.borrow_mut().eoi(vector);
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );
    let pba_bits = m.read_physical_u64(bar0_base + pba_offset);
    assert_eq!(
        pba_bits & 1,
        0,
        "expected MSI-X pending bit 0 to clear after unmask + delivery"
    );
}

#[test]
fn snapshot_restore_preserves_nvme_msix_vector_mask_pending_bit_and_delivers_after_unmask() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_nvme: true,
        // Keep the test focused on NVMe + snapshot + per-vector MSI-X mask semantics.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    // Ensure high MMIO addresses decode correctly (avoid A20 aliasing).
    m.io_write(A20_GATE_PORT, 1, 0x02);

    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    interrupts
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);
    assert_eq!(interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    let bdf = profile::NVME_CONTROLLER.bdf;

    // Enable PCI memory decoding + bus mastering (required for MMIO + DMA).
    let cmd = cfg_read(&mut m, bdf, 0x04, 2) as u16;
    cfg_write(&mut m, bdf, 0x04, 2, u32::from(cmd | (1 << 1) | (1 << 2)));

    // Read BAR0 base (64-bit MMIO BAR).
    let bar0_lo = cfg_read(&mut m, bdf, 0x10, 4) as u64;
    let bar0_hi = cfg_read(&mut m, bdf, 0x14, 4) as u64;
    let bar0_base = (bar0_hi << 32) | (bar0_lo & !0xFu64);
    assert_ne!(
        bar0_base, 0,
        "expected NVMe BAR0 to be assigned during BIOS POST"
    );

    // Locate MSI-X capability and validate table/PBA live in BAR0.
    let msix_cap = find_capability(&mut m, bdf, aero_devices::pci::msix::PCI_CAP_ID_MSIX)
        .expect("NVMe should expose MSI-X capability");
    let table = cfg_read(&mut m, bdf, msix_cap + 0x04, 4);
    let pba = cfg_read(&mut m, bdf, msix_cap + 0x08, 4);
    assert_eq!(table & 0x7, 0, "MSI-X table must live in BAR0 (BIR=0)");
    assert_eq!(pba & 0x7, 0, "MSI-X PBA must live in BAR0 (BIR=0)");
    let table_offset = u64::from(table & !0x7);
    let pba_offset = u64::from(pba & !0x7);

    // Program MSI-X table entry 0, but keep the entry masked (vector control bit 0).
    let vector: u8 = 0x6b;
    let entry0 = bar0_base + table_offset;
    m.write_physical_u32(entry0, 0xfee0_0000);
    m.write_physical_u32(entry0 + 0x4, 0);
    m.write_physical_u32(entry0 + 0x8, u32::from(vector));
    m.write_physical_u32(entry0 + 0xc, 1); // masked

    // Enable MSI-X (bit 15) and ensure function mask (bit 14) is cleared.
    let ctrl = cfg_read(&mut m, bdf, msix_cap + 0x02, 2) as u16;
    cfg_write(
        &mut m,
        bdf,
        msix_cap + 0x02,
        2,
        u32::from((ctrl & !(1 << 14)) | (1 << 15)),
    );

    // Trigger a completion while the MSI-X entry is masked (admin IDENTIFY).
    let asq = 0x10000u64;
    let acq = 0x20000u64;
    let id_buf = 0x30000u64;

    m.write_physical_u32(bar0_base + 0x0024, 0x000f_000f); // AQA
    m.write_physical_u64(bar0_base + 0x0028, asq); // ASQ
    m.write_physical_u64(bar0_base + 0x0030, acq); // ACQ
    m.write_physical_u32(bar0_base + 0x0014, 1); // CC.EN

    let mut cmd = [0u8; 64];
    cmd[0] = 0x06; // IDENTIFY
    cmd[2..4].copy_from_slice(&0x1234u16.to_le_bytes()); // CID
    cmd[24..32].copy_from_slice(&id_buf.to_le_bytes()); // PRP1
    cmd[40..44].copy_from_slice(&0x01u32.to_le_bytes()); // CDW10: CNS=1 (controller)
    m.write_physical(asq, &cmd);

    // Ring SQ0 tail doorbell.
    m.write_physical_u32(bar0_base + 0x1000, 1);

    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );
    m.process_nvme();

    let nvme = m.nvme().expect("nvme enabled");
    assert!(
        !nvme.borrow().irq_level(),
        "NVMe should not assert legacy INTx while MSI-X is enabled (even if the entry is masked)"
    );
    assert!(
        nvme.borrow().irq_pending(),
        "expected NVMe to have an interrupt pending (completion posted)"
    );
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None,
        "expected no MSI-X delivery while the entry is masked"
    );
    let pba_bits = m.read_physical_u64(bar0_base + pba_offset);
    assert_ne!(
        pba_bits & 1,
        0,
        "expected MSI-X pending bit 0 to be set while the entry is masked"
    );

    // Clear the NVMe interrupt condition before snapshotting by consuming the completion entry
    // (advance CQ0 head). Pending MSI-X delivery must still occur later due to the PBA pending bit.
    m.write_physical_u32(bar0_base + 0x1004, 1); // CQ0 head = 1
    assert!(
        !nvme.borrow().irq_pending(),
        "expected NVMe interrupt condition to be cleared after consuming CQ0 completion"
    );
    assert_ne!(
        m.read_physical_u64(bar0_base + pba_offset) & 1,
        0,
        "expected MSI-X pending bit 0 to remain set after clearing interrupt condition"
    );

    let snapshot = m.take_snapshot_full().unwrap();

    // Mutate state after snapshot: unmask the entry and observe delivery + pending-bit clear.
    m.write_physical_u32(entry0 + 0xc, 0);
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        Some(vector)
    );
    interrupts.borrow_mut().acknowledge(vector);
    interrupts.borrow_mut().eoi(vector);
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );
    let pba_bits = m.read_physical_u64(bar0_base + pba_offset);
    assert_eq!(
        pba_bits & 1,
        0,
        "expected pending bit to clear after unmask + delivery"
    );

    m.restore_snapshot_bytes(&snapshot).unwrap();

    // Ensure high MMIO addresses decode correctly post-restore.
    m.io_write(A20_GATE_PORT, 1, 0x02);

    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    assert_eq!(interrupts.borrow().mode(), PlatformInterruptMode::Apic);
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );

    // MSI-X should still be enabled, and the function mask should still be cleared.
    let ctrl_restored = cfg_read(&mut m, bdf, msix_cap + 0x02, 2) as u16;
    assert_ne!(
        ctrl_restored & (1 << 15),
        0,
        "expected MSI-X enable bit restored"
    );
    assert_eq!(
        ctrl_restored & (1 << 14),
        0,
        "expected MSI-X function mask bit restored as cleared"
    );

    // Ensure MSI-X table entry mask + PBA pending bit were restored.
    let bar0_lo = cfg_read(&mut m, bdf, 0x10, 4) as u64;
    let bar0_hi = cfg_read(&mut m, bdf, 0x14, 4) as u64;
    let bar0_base = (bar0_hi << 32) | (bar0_lo & !0xFu64);
    let entry0 = bar0_base + table_offset;
    assert_eq!(
        m.read_physical_u32(entry0 + 0xc) & 1,
        1,
        "expected MSI-X vector control mask bit restored"
    );
    let pba_bits = m.read_physical_u64(bar0_base + pba_offset);
    assert_ne!(
        pba_bits & 1,
        0,
        "expected MSI-X pending bit 0 to survive snapshot/restore"
    );
    let nvme = m.nvme().expect("nvme enabled");
    assert!(
        !nvme.borrow().irq_pending(),
        "expected NVMe interrupt condition to remain cleared across snapshot/restore"
    );

    // Unmask after restore and expect immediate delivery (and pending-bit clear).
    m.write_physical_u32(entry0 + 0xc, 0);
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        Some(vector)
    );
    interrupts.borrow_mut().acknowledge(vector);
    interrupts.borrow_mut().eoi(vector);
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );
    let pba_bits = m.read_physical_u64(bar0_base + pba_offset);
    assert_eq!(
        pba_bits & 1,
        0,
        "expected MSI-X pending bit 0 to clear after restore + unmask + delivery"
    );
}
