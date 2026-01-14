use aero_devices::pci::msix::PCI_CAP_ID_MSIX;
use aero_devices::pci::profile::{
    VIRTIO_BAR0_SIZE, VIRTIO_BLK, VIRTIO_COMMON_CFG_BAR0_OFFSET, VIRTIO_NOTIFY_CFG_BAR0_OFFSET,
};
use aero_devices::pci::{PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_io_snapshot::io::state::IoSnapshot;
use aero_pc_platform::{PcPlatform, PcPlatformConfig};
use aero_platform::interrupts::{
    InterruptController, PlatformInterruptMode, IMCR_DATA_PORT, IMCR_INDEX, IMCR_SELECT_PORT,
};
use memory::MemoryBus as _;

fn cfg_addr(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | (offset as u32 & 0xFC)
}

fn cfg_data_port(offset: u8) -> u16 {
    PCI_CFG_DATA_PORT + u16::from(offset & 0x3)
}

fn read_cfg_u8(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8) -> u8 {
    pc.io
        .write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bus, device, function, offset));
    pc.io.read(cfg_data_port(offset), 1) as u8
}

fn read_cfg_u16(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8) -> u16 {
    pc.io
        .write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bus, device, function, offset));
    pc.io.read(cfg_data_port(offset), 2) as u16
}

fn read_cfg_u32(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    pc.io
        .write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bus, device, function, offset));
    pc.io.read(cfg_data_port(offset), 4)
}

fn write_cfg_u16(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    pc.io
        .write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bus, device, function, offset));
    pc.io.write(cfg_data_port(offset), 2, u32::from(value));
}

fn find_capability(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, id: u8) -> Option<u8> {
    let mut offset = read_cfg_u8(pc, bus, device, function, 0x34);
    for _ in 0..64 {
        if offset == 0 {
            return None;
        }
        let cap_id = read_cfg_u8(pc, bus, device, function, offset);
        if cap_id == id {
            return Some(offset);
        }
        offset = read_cfg_u8(pc, bus, device, function, offset.wrapping_add(1));
    }
    None
}

fn read_bar0_base(pc: &mut PcPlatform) -> u64 {
    let bdf = VIRTIO_BLK.bdf;
    let lo = read_cfg_u32(pc, bdf.bus, bdf.device, bdf.function, 0x10);
    let hi = read_cfg_u32(pc, bdf.bus, bdf.device, bdf.function, 0x14);
    (u64::from(hi) << 32) | u64::from(lo & 0xffff_fff0)
}

fn write_desc(
    pc: &mut PcPlatform,
    table: u64,
    index: u16,
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
) {
    let base = table + u64::from(index) * 16;
    pc.memory.write_u64(base, addr);
    pc.memory.write_u32(base + 8, len);
    pc.memory.write_u16(base + 12, flags);
    pc.memory.write_u16(base + 14, next);
}

#[test]
fn pc_platform_virtio_blk_msix_snapshot_restore_preserves_msix_state() {
    const RAM_SIZE: usize = 2 * 1024 * 1024;
    let config = PcPlatformConfig {
        enable_ahci: false,
        enable_uhci: false,
        enable_virtio_blk: true,
        enable_virtio_msix: true,
        ..Default::default()
    };
    let mut pc = PcPlatform::new_with_config(RAM_SIZE, config);
    let bdf = VIRTIO_BLK.bdf;

    // Switch to APIC mode so MSI delivery targets the LAPIC.
    pc.io.write_u8(IMCR_SELECT_PORT, IMCR_INDEX);
    pc.io.write_u8(IMCR_DATA_PORT, 0x01);
    assert_eq!(pc.interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    // Enable BAR0 MMIO decode + bus mastering, and disable legacy INTx so only MSI-X delivery is
    // observable.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0406);

    let bar0_base = read_bar0_base(&mut pc);
    assert_ne!(bar0_base, 0);
    assert_eq!(bar0_base % VIRTIO_BAR0_SIZE, 0);

    // Locate + enable MSI-X.
    let msix_cap = find_capability(&mut pc, bdf.bus, bdf.device, bdf.function, PCI_CAP_ID_MSIX)
        .expect("virtio-blk should expose MSI-X when enabled");
    let ctrl = read_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, msix_cap + 0x02);
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        msix_cap + 0x02,
        ctrl | (1 << 15),
    );
    let ctrl2 = read_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, msix_cap + 0x02);
    assert_ne!(ctrl2 & (1 << 15), 0);

    // Program MSI-X table entry 0: destination = BSP (APIC ID 0), vector = 0x55.
    let table = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, msix_cap + 0x04);
    assert_eq!(table & 0x7, 0, "MSI-X table should live in BAR0");
    let table_offset = u64::from(table & !0x7);
    let entry0 = bar0_base + table_offset;
    pc.memory.write_u32(entry0 + 0x0, 0xfee0_0000);
    pc.memory.write_u32(entry0 + 0x4, 0);
    pc.memory.write_u32(entry0 + 0x8, 0x0055);
    pc.memory.write_u32(entry0 + 0xc, 0); // unmasked

    // BAR0 layout for Aero's virtio-pci contract:
    // - common cfg @ 0x0000
    // - notify @ 0x1000
    const COMMON: u64 = VIRTIO_COMMON_CFG_BAR0_OFFSET as u64;
    const NOTIFY: u64 = VIRTIO_NOTIFY_CFG_BAR0_OFFSET as u64;

    // Basic feature negotiation (accept whatever the device offers).
    pc.memory.write_u8(bar0_base + COMMON + 0x14, 1); // ACKNOWLEDGE
    pc.memory.write_u8(bar0_base + COMMON + 0x14, 1 | 2); // ACKNOWLEDGE | DRIVER

    pc.memory.write_u32(bar0_base + COMMON, 0);
    let f0 = pc.memory.read_u32(bar0_base + COMMON + 0x04);
    pc.memory.write_u32(bar0_base + COMMON + 0x08, 0);
    pc.memory.write_u32(bar0_base + COMMON + 0x0c, f0);

    pc.memory.write_u32(bar0_base + COMMON, 1);
    let f1 = pc.memory.read_u32(bar0_base + COMMON + 0x04);
    pc.memory.write_u32(bar0_base + COMMON + 0x08, 1);
    pc.memory.write_u32(bar0_base + COMMON + 0x0c, f1);

    pc.memory.write_u8(bar0_base + COMMON + 0x14, 1 | 2 | 8); // + FEATURES_OK
    pc.memory.write_u8(bar0_base + COMMON + 0x14, 1 | 2 | 8 | 4); // + DRIVER_OK

    // Configure queue 0 with MSI-X vector 0.
    const DESC_TABLE: u64 = 0x4000;
    const AVAIL_RING: u64 = 0x5000;
    const USED_RING: u64 = 0x6000;
    pc.memory.write_u16(bar0_base + COMMON + 0x16, 0); // queue_select
    let qsz = pc.memory.read_u16(bar0_base + COMMON + 0x18);
    assert!(qsz >= 2);

    pc.memory.write_u16(bar0_base + COMMON + 0x1a, 0); // queue_msix_vector
    assert_eq!(pc.memory.read_u16(bar0_base + COMMON + 0x1a), 0);

    pc.memory.write_u64(bar0_base + COMMON + 0x20, DESC_TABLE);
    pc.memory.write_u64(bar0_base + COMMON + 0x28, AVAIL_RING);
    pc.memory.write_u64(bar0_base + COMMON + 0x30, USED_RING);
    pc.memory.write_u16(bar0_base + COMMON + 0x1c, 1); // queue_enable

    // Build a minimal FLUSH request.
    const VIRTIO_BLK_T_FLUSH: u32 = 4;
    const VIRTQ_DESC_F_NEXT: u16 = 0x0001;
    const VIRTQ_DESC_F_WRITE: u16 = 0x0002;
    let header = 0x7000;
    let status = 0x9000;
    pc.memory.write_u32(header, VIRTIO_BLK_T_FLUSH);
    pc.memory.write_u32(header + 4, 0);
    pc.memory.write_u64(header + 8, 0);
    pc.memory.write_u8(status, 0xff);

    write_desc(&mut pc, DESC_TABLE, 0, header, 16, VIRTQ_DESC_F_NEXT, 1);
    write_desc(&mut pc, DESC_TABLE, 1, status, 1, VIRTQ_DESC_F_WRITE, 0);

    // avail.flags=0, avail.idx=1, avail.ring[0]=0
    pc.memory.write_u16(AVAIL_RING, 0);
    pc.memory.write_u16(AVAIL_RING + 2, 1);
    pc.memory.write_u16(AVAIL_RING + 4, 0);
    pc.memory.write_u16(USED_RING, 0);
    pc.memory.write_u16(USED_RING + 2, 0);

    // Doorbell, but snapshot before the platform processes the queue.
    pc.memory.write_u16(bar0_base + NOTIFY, 0);

    let dev_snap = pc.virtio_blk.as_ref().unwrap().borrow().save_state();
    let pci_snap = pc.pci_cfg.borrow().save_state();
    let mut ram_img = vec![0u8; RAM_SIZE];
    pc.memory.read_physical(0, &mut ram_img);

    // Restore into a fresh platform.
    let mut restored = PcPlatform::new_with_config(RAM_SIZE, config);
    restored.memory.write_physical(0, &ram_img);
    restored.pci_cfg.borrow_mut().load_state(&pci_snap).unwrap();
    restored
        .virtio_blk
        .as_ref()
        .unwrap()
        .borrow_mut()
        .load_state(&dev_snap)
        .unwrap();

    // Put the restored platform into APIC mode (interrupt controller state is not part of these
    // manual snapshots).
    restored.io.write_u8(IMCR_SELECT_PORT, IMCR_INDEX);
    restored.io.write_u8(IMCR_DATA_PORT, 0x01);
    assert_eq!(restored.interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    // Verify MSI-X enable bit is preserved in guest-visible PCI config space.
    let msix_cap2 =
        find_capability(&mut restored, bdf.bus, bdf.device, bdf.function, PCI_CAP_ID_MSIX)
            .expect("restored virtio-blk should still expose MSI-X");
    let ctrl_restored =
        read_cfg_u16(&mut restored, bdf.bus, bdf.device, bdf.function, msix_cap2 + 0x02);
    assert_ne!(
        ctrl_restored & (1 << 15),
        0,
        "MSI-X enable bit should be preserved across snapshot/restore"
    );

    // Verify MSI-X table contents are preserved.
    let bar0_base2 = read_bar0_base(&mut restored);
    let table2 = read_cfg_u32(&mut restored, bdf.bus, bdf.device, bdf.function, msix_cap2 + 0x04);
    let table_offset2 = u64::from(table2 & !0x7);
    let entry0_2 = bar0_base2 + table_offset2;
    assert_eq!(restored.memory.read_u32(entry0_2 + 0x0), 0xfee0_0000);
    assert_eq!(restored.memory.read_u32(entry0_2 + 0x4), 0);
    assert_eq!(restored.memory.read_u32(entry0_2 + 0x8), 0x0055);
    assert_eq!(restored.memory.read_u32(entry0_2 + 0xc) & 1, 0);

    // Ensure the queue MSI-X vector is still assigned.
    restored
        .memory
        .write_u16(bar0_base2 + COMMON + 0x16, 0); // queue_select
    assert_eq!(
        restored.memory.read_u16(bar0_base2 + COMMON + 0x1a),
        0
    );

    assert_eq!(restored.interrupts.borrow().get_pending(), None);

    // Process the pending queue entry. This should complete the request and inject an MSI-X vector.
    restored.process_virtio_blk();
    assert_eq!(restored.memory.read_u16(USED_RING + 2), 1);
    assert_eq!(restored.memory.read_u8(status), 0);
    assert_eq!(restored.interrupts.borrow().get_pending(), Some(0x55));
}
