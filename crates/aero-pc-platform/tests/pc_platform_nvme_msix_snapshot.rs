mod helpers;

use aero_devices::pci::msix::PCI_CAP_ID_MSIX;
use aero_devices::pci::profile::NVME_CONTROLLER;
use aero_devices::pci::PciBdf;
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

fn configure_and_enable_nvme_controller(pc: &mut PcPlatform, bar0_base: u64, asq: u64, acq: u64) {
    // Configure + enable controller.
    pc.memory.write_u32(bar0_base + 0x0024, 0x000f_000f); // AQA
    pc.memory.write_u64(bar0_base + 0x0028, asq); // ASQ
    pc.memory.write_u64(bar0_base + 0x0030, acq); // ACQ
    pc.memory.write_u32(bar0_base + 0x0014, 1); // CC.EN
}

fn submit_admin_identify(pc: &mut PcPlatform, bar0_base: u64, asq: u64, id_buf: u64) {
    // Admin IDENTIFY (controller) command in SQ0 entry 0.
    let mut cmd = [0u8; 64];
    cmd[0] = 0x06; // IDENTIFY
    cmd[2..4].copy_from_slice(&0x1234u16.to_le_bytes()); // CID
    cmd[24..32].copy_from_slice(&id_buf.to_le_bytes()); // PRP1
    cmd[40..44].copy_from_slice(&0x01u32.to_le_bytes()); // CDW10: CNS=1 (controller)
    pc.memory.write_physical(asq, &cmd);

    // Ring SQ0 tail doorbell.
    pc.memory.write_u32(bar0_base + 0x1000, 1);
}

#[test]
fn pc_platform_nvme_msix_snapshot_restore_preserves_vector_mask_pending_and_delivers_after_unmask() {
    const RAM_SIZE: usize = 2 * 1024 * 1024;
    let config = PcPlatformConfig {
        enable_nvme: true,
        enable_ahci: false,
        enable_uhci: false,
        ..Default::default()
    };
    let mut pc = PcPlatform::new_with_config(RAM_SIZE, config);
    let bdf = NVME_CONTROLLER.bdf;

    // Switch to APIC mode so MSI-X delivery targets the LAPIC.
    pc.io.write_u8(IMCR_SELECT_PORT, IMCR_INDEX);
    pc.io.write_u8(IMCR_DATA_PORT, 0x01);
    assert_eq!(pc.interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    // Enable BAR0 MMIO decode + bus mastering (leave INTx enabled so we can validate no INTx
    // fallback while MSI-X is active).
    let mut cmd = pci_cfg_read_u16(&mut pc, bdf, 0x04);
    cmd |= (1 << 1) | (1 << 2);
    cmd &= !(1 << 10);
    pci_cfg_write_u16(&mut pc, bdf, 0x04, cmd);

    let bar0_base = pci_read_bar(&mut pc, bdf, 0).base;
    assert_ne!(bar0_base, 0);

    // Locate MSI-X capability and discover table/PBA offsets.
    let msix_cap = find_capability(&mut pc, bdf, PCI_CAP_ID_MSIX)
        .expect("NVMe should expose MSI-X capability");
    let msix_base = u16::from(msix_cap);
    let table = pci_cfg_read_u32(&mut pc, bdf, msix_base + 0x04);
    let pba = pci_cfg_read_u32(&mut pc, bdf, msix_base + 0x08);
    assert_eq!(table & 0x7, 0, "NVMe MSI-X table should live in BAR0 (BIR=0)");
    assert_eq!(pba & 0x7, 0, "NVMe MSI-X PBA should live in BAR0 (BIR=0)");
    let table_offset = u64::from(table & !0x7);
    let pba_offset = u64::from(pba & !0x7);

    // Program table entry 0 (masked).
    let vector: u16 = 0x006a;
    let entry0 = bar0_base + table_offset;
    pc.memory.write_u32(entry0, 0xfee0_0000);
    pc.memory.write_u32(entry0 + 0x4, 0);
    pc.memory.write_u32(entry0 + 0x8, u32::from(vector));
    pc.memory.write_u32(entry0 + 0xc, 1); // masked

    // Enable MSI-X (bit 15) and ensure Function Mask (bit 14) is cleared.
    let ctrl = pci_cfg_read_u16(&mut pc, bdf, msix_base + 0x02);
    pci_cfg_write_u16(
        &mut pc,
        bdf,
        msix_base + 0x02,
        (ctrl & !(1 << 14)) | (1 << 15),
    );

    // Set up admin queue and submit an IDENTIFY command.
    let asq = 0x10000u64;
    let acq = 0x20000u64;
    let id_buf = 0x30000u64;
    configure_and_enable_nvme_controller(&mut pc, bar0_base, asq, acq);
    submit_admin_identify(&mut pc, bar0_base, asq, id_buf);
    pc.process_nvme();

    // The NVMe controller should have an interrupt condition (completion pending), but MSI-X
    // delivery should be suppressed while the vector is masked. The MSI-X PBA pending bit must
    // latch without falling back to INTx.
    assert!(
        pc.nvme.as_ref().unwrap().borrow().irq_pending(),
        "expected NVMe interrupt condition to be asserted after completion"
    );
    assert!(
        !pc.nvme.as_ref().unwrap().borrow().irq_level(),
        "legacy INTx should be suppressed while MSI-X is enabled (even if the entry is masked)"
    );
    assert_eq!(
        pc.interrupts.borrow().get_pending(),
        None,
        "expected no MSI-X delivery while the MSI-X entry is masked"
    );
    assert_ne!(
        pc.memory.read_u64(bar0_base + pba_offset) & 1,
        0,
        "expected MSI-X pending bit 0 to be set while the entry is masked"
    );

    // Consume the completion by advancing CQ0 head. This clears the interrupt condition; the PBA
    // pending bit should remain set.
    pc.memory.write_u32(bar0_base + 0x1004, 1); // CQ0 head = 1
    assert!(
        !pc.nvme.as_ref().unwrap().borrow().irq_pending(),
        "expected interrupt condition to be cleared after consuming CQ0 completion"
    );
    assert_ne!(
        pc.memory.read_u64(bar0_base + pba_offset) & 1,
        0,
        "expected MSI-X pending bit 0 to remain set after clearing the interrupt condition"
    );

    // Snapshot device + PCI config + guest RAM.
    let dev_snap = pc.nvme.as_ref().unwrap().borrow().save_state();
    let pci_snap = pc.pci_cfg.borrow().save_state();
    let mut ram_img = vec![0u8; RAM_SIZE];
    pc.memory.read_physical(0, &mut ram_img);

    // Restore into a fresh platform.
    let mut restored = PcPlatform::new_with_config(RAM_SIZE, config);
    restored.memory.write_physical(0, &ram_img);
    restored.pci_cfg.borrow_mut().load_state(&pci_snap).unwrap();
    restored
        .nvme
        .as_ref()
        .unwrap()
        .borrow_mut()
        .load_state(&dev_snap)
        .unwrap();

    // Switch restored platform to APIC mode (interrupt controller state is not part of these
    // manual snapshots).
    restored.io.write_u8(IMCR_SELECT_PORT, IMCR_INDEX);
    restored.io.write_u8(IMCR_DATA_PORT, 0x01);
    assert_eq!(
        restored.interrupts.borrow().mode(),
        PlatformInterruptMode::Apic
    );

    let bar0_base2 = pci_read_bar(&mut restored, bdf, 0).base;
    assert_ne!(bar0_base2, 0);

    // Ensure the MSI-X entry mask + PBA pending bit survived restore, and the interrupt condition
    // is still cleared (delivery should be driven solely by PBA pending).
    let entry0_2 = bar0_base2 + table_offset;
    assert_eq!(
        restored.memory.read_u32(entry0_2 + 0xc) & 1,
        1,
        "expected MSI-X vector control mask bit to be restored"
    );
    assert_ne!(
        restored.memory.read_u64(bar0_base2 + pba_offset) & 1,
        0,
        "expected MSI-X pending bit 0 to survive snapshot/restore"
    );
    assert!(
        !restored.nvme.as_ref().unwrap().borrow().irq_pending(),
        "expected interrupt condition to remain cleared after restore"
    );
    assert_eq!(restored.interrupts.borrow().get_pending(), None);

    // Unmask after restore. This should immediately deliver the pending vector and clear the PBA,
    // even without a new NVMe interrupt condition.
    restored.memory.write_u32(entry0_2 + 0xc, 0);
    assert_eq!(
        restored.interrupts.borrow().get_pending(),
        Some(vector as u8)
    );
    restored.interrupts.borrow_mut().acknowledge(vector as u8);
    restored.interrupts.borrow_mut().eoi(vector as u8);
    assert_eq!(restored.interrupts.borrow().get_pending(), None);
    assert_eq!(
        restored.memory.read_u64(bar0_base2 + pba_offset) & 1,
        0,
        "expected MSI-X pending bit 0 to clear after restore + unmask + delivery"
    );
}

#[test]
fn pc_platform_nvme_msix_snapshot_restore_preserves_function_mask_pending_and_delivers_after_unmask()
{
    const RAM_SIZE: usize = 2 * 1024 * 1024;
    let config = PcPlatformConfig {
        enable_nvme: true,
        enable_ahci: false,
        enable_uhci: false,
        ..Default::default()
    };
    let mut pc = PcPlatform::new_with_config(RAM_SIZE, config);
    let bdf = NVME_CONTROLLER.bdf;

    // Switch to APIC mode so MSI-X delivery targets the LAPIC.
    pc.io.write_u8(IMCR_SELECT_PORT, IMCR_INDEX);
    pc.io.write_u8(IMCR_DATA_PORT, 0x01);
    assert_eq!(pc.interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    // Enable BAR0 MMIO decode + bus mastering (leave INTx enabled so we can validate no INTx
    // fallback while MSI-X is active).
    let mut cmd = pci_cfg_read_u16(&mut pc, bdf, 0x04);
    cmd |= (1 << 1) | (1 << 2);
    cmd &= !(1 << 10);
    pci_cfg_write_u16(&mut pc, bdf, 0x04, cmd);

    let bar0_base = pci_read_bar(&mut pc, bdf, 0).base;
    assert_ne!(bar0_base, 0);

    // Locate MSI-X capability and discover table/PBA offsets.
    let msix_cap = find_capability(&mut pc, bdf, PCI_CAP_ID_MSIX)
        .expect("NVMe should expose MSI-X capability");
    let msix_base = u16::from(msix_cap);
    let table = pci_cfg_read_u32(&mut pc, bdf, msix_base + 0x04);
    let pba = pci_cfg_read_u32(&mut pc, bdf, msix_base + 0x08);
    assert_eq!(table & 0x7, 0, "NVMe MSI-X table should live in BAR0 (BIR=0)");
    assert_eq!(pba & 0x7, 0, "NVMe MSI-X PBA should live in BAR0 (BIR=0)");
    let table_offset = u64::from(table & !0x7);
    let pba_offset = u64::from(pba & !0x7);

    // Program table entry 0 (unmasked).
    let vector: u16 = 0x006b;
    let entry0 = bar0_base + table_offset;
    pc.memory.write_u32(entry0, 0xfee0_0000);
    pc.memory.write_u32(entry0 + 0x4, 0);
    pc.memory.write_u32(entry0 + 0x8, u32::from(vector));
    pc.memory.write_u32(entry0 + 0xc, 0);

    // Enable MSI-X and set Function Mask (bit 14).
    let ctrl = pci_cfg_read_u16(&mut pc, bdf, msix_base + 0x02);
    pci_cfg_write_u16(
        &mut pc,
        bdf,
        msix_base + 0x02,
        ctrl | (1 << 15) | (1 << 14),
    );

    // Set up admin queue and submit an IDENTIFY command.
    let asq = 0x10000u64;
    let acq = 0x20000u64;
    let id_buf = 0x30000u64;
    configure_and_enable_nvme_controller(&mut pc, bar0_base, asq, acq);
    submit_admin_identify(&mut pc, bar0_base, asq, id_buf);
    pc.process_nvme();

    // Function mask should suppress MSI-X delivery but latch PBA pending. There must be no INTx
    // fallback while MSI-X is enabled.
    assert!(
        pc.nvme.as_ref().unwrap().borrow().irq_pending(),
        "expected NVMe interrupt condition to be asserted after completion"
    );
    assert!(
        !pc.nvme.as_ref().unwrap().borrow().irq_level(),
        "legacy INTx should be suppressed while MSI-X is enabled (even if function-masked)"
    );
    assert_eq!(
        pc.interrupts.borrow().get_pending(),
        None,
        "expected no MSI-X delivery while Function Mask is set"
    );
    assert_ne!(
        pc.memory.read_u64(bar0_base + pba_offset) & 1,
        0,
        "expected MSI-X pending bit 0 to be set while Function Mask is set"
    );

    // Consume the completion by advancing CQ0 head, clearing the interrupt condition. Pending
    // delivery should still occur once Function Mask is cleared.
    pc.memory.write_u32(bar0_base + 0x1004, 1); // CQ0 head = 1
    assert!(
        !pc.nvme.as_ref().unwrap().borrow().irq_pending(),
        "expected interrupt condition to be cleared after consuming CQ0 completion"
    );
    assert_ne!(
        pc.memory.read_u64(bar0_base + pba_offset) & 1,
        0,
        "expected MSI-X pending bit 0 to remain set after clearing the interrupt condition"
    );

    // Snapshot device + PCI config + guest RAM.
    let dev_snap = pc.nvme.as_ref().unwrap().borrow().save_state();
    let pci_snap = pc.pci_cfg.borrow().save_state();
    let mut ram_img = vec![0u8; RAM_SIZE];
    pc.memory.read_physical(0, &mut ram_img);

    // Restore into a fresh platform.
    let mut restored = PcPlatform::new_with_config(RAM_SIZE, config);
    restored.memory.write_physical(0, &ram_img);
    restored.pci_cfg.borrow_mut().load_state(&pci_snap).unwrap();
    restored
        .nvme
        .as_ref()
        .unwrap()
        .borrow_mut()
        .load_state(&dev_snap)
        .unwrap();

    // Switch restored platform to APIC mode (interrupt controller state is not part of these
    // manual snapshots).
    restored.io.write_u8(IMCR_SELECT_PORT, IMCR_INDEX);
    restored.io.write_u8(IMCR_DATA_PORT, 0x01);
    assert_eq!(
        restored.interrupts.borrow().mode(),
        PlatformInterruptMode::Apic
    );

    let bar0_base2 = pci_read_bar(&mut restored, bdf, 0).base;
    assert_ne!(bar0_base2, 0);

    // Ensure the PBA pending bit survived restore and the interrupt condition is still cleared.
    assert_ne!(
        restored.memory.read_u64(bar0_base2 + pba_offset) & 1,
        0,
        "expected MSI-X pending bit 0 to survive snapshot/restore"
    );
    assert!(
        !restored.nvme.as_ref().unwrap().borrow().irq_pending(),
        "expected interrupt condition to remain cleared after restore"
    );
    assert_eq!(restored.interrupts.borrow().get_pending(), None);

    // Clear Function Mask in PCI config space and process once to synchronize canonical config into
    // the device model. The pending MSI-X vector should deliver even without a new interrupt edge.
    let ctrl_restored = pci_cfg_read_u16(&mut restored, bdf, msix_base + 0x02);
    pci_cfg_write_u16(
        &mut restored,
        bdf,
        msix_base + 0x02,
        ctrl_restored & !(1 << 14),
    );
    restored.process_nvme();

    assert_eq!(
        restored.interrupts.borrow().get_pending(),
        Some(vector as u8)
    );
    restored.interrupts.borrow_mut().acknowledge(vector as u8);
    restored.interrupts.borrow_mut().eoi(vector as u8);
    assert_eq!(restored.interrupts.borrow().get_pending(), None);
    assert_eq!(
        restored.memory.read_u64(bar0_base2 + pba_offset) & 1,
        0,
        "expected MSI-X pending bit 0 to clear after restore + unmask + delivery"
    );
}

