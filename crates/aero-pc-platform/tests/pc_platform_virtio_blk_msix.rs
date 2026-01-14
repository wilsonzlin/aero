use aero_devices::pci::msix::PCI_CAP_ID_MSIX;
use aero_devices::pci::profile::{
    VIRTIO_BAR0_SIZE, VIRTIO_BLK, VIRTIO_COMMON_CFG_BAR0_OFFSET, VIRTIO_ISR_CFG_BAR0_OFFSET,
    VIRTIO_NOTIFY_CFG_BAR0_OFFSET,
};
use aero_devices::pci::{PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
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
    pc.io.write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bus, device, function, offset),
    );
    pc.io.read(cfg_data_port(offset), 1) as u8
}

fn read_cfg_u16(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8) -> u16 {
    pc.io.write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bus, device, function, offset),
    );
    pc.io.read(cfg_data_port(offset), 2) as u16
}

fn read_cfg_u32(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    pc.io.write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bus, device, function, offset),
    );
    pc.io.read(cfg_data_port(offset), 4)
}

fn write_cfg_u16(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    pc.io.write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bus, device, function, offset),
    );
    pc.io.write(cfg_data_port(offset), 2, u32::from(value));
}

fn find_capability(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, id: u8) -> Option<u8> {
    let mut offset = read_cfg_u8(pc, bus, device, function, 0x34);
    // Capabilities are inside the first 256 bytes; guard against loops.
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
fn pc_platform_virtio_blk_msix_triggers_lapic_vector() {
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            enable_ahci: false,
            enable_uhci: false,
            enable_virtio_blk: true,
            enable_virtio_msix: true,
            ..Default::default()
        },
    );
    let bdf = VIRTIO_BLK.bdf;

    // Switch into APIC mode so we can observe LAPIC pending vectors via `InterruptController`.
    pc.io.write_u8(IMCR_SELECT_PORT, IMCR_INDEX);
    pc.io.write_u8(IMCR_DATA_PORT, 0x01);
    assert_eq!(pc.interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    // Enable BAR0 MMIO decode + bus mastering, and disable legacy INTx so MSI-X delivery is
    // required for interrupts to be observed.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0406);

    let bar0_base = read_bar0_base(&mut pc);
    assert_ne!(bar0_base, 0);
    assert_eq!(bar0_base % VIRTIO_BAR0_SIZE, 0);

    // Locate the MSI-X capability.
    let msix_cap = find_capability(&mut pc, bdf.bus, bdf.device, bdf.function, PCI_CAP_ID_MSIX)
        .expect("virtio-blk should expose an MSI-X capability when enabled");

    // Table offset/BIR must point into BAR0.
    let table = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, msix_cap + 0x04);
    assert_eq!(table & 0x7, 0, "MSI-X table should live in BAR0 (BIR=0)");
    let table_offset = u64::from(table & !0x7);

    // Enable MSI-X.
    let ctrl = read_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, msix_cap + 0x02);
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        msix_cap + 0x02,
        ctrl | (1 << 15),
    );

    // Program table entry 0: destination = BSP (APIC ID 0), vector = 0x55.
    let entry0 = bar0_base + table_offset;
    pc.memory.write_u32(entry0, 0xfee0_0000);
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

    // Configure queue 0 and route its interrupts to MSI-X vector 0.
    const DESC_TABLE: u64 = 0x4000;
    const AVAIL_RING: u64 = 0x5000;
    const USED_RING: u64 = 0x6000;
    pc.memory.write_u16(bar0_base + COMMON + 0x16, 0); // queue_select
    let qsz = pc.memory.read_u16(bar0_base + COMMON + 0x18);
    assert!(qsz >= 2);

    // queue_msix_vector
    pc.memory.write_u16(bar0_base + COMMON + 0x1a, 0);
    assert_eq!(
        pc.memory.read_u16(bar0_base + COMMON + 0x1a),
        0,
        "queue_msix_vector should be writable after MSI-X is enabled"
    );

    pc.memory.write_u64(bar0_base + COMMON + 0x20, DESC_TABLE);
    pc.memory.write_u64(bar0_base + COMMON + 0x28, AVAIL_RING);
    pc.memory.write_u64(bar0_base + COMMON + 0x30, USED_RING);
    pc.memory.write_u16(bar0_base + COMMON + 0x1c, 1); // queue_enable

    // Build a minimal FLUSH request (no data buffers needed).
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

    assert_eq!(pc.interrupts.borrow().get_pending(), None);

    pc.memory.write_u16(bar0_base + NOTIFY, 0);
    pc.process_virtio_blk();

    assert_eq!(pc.memory.read_u16(USED_RING + 2), 1);
    assert_eq!(pc.memory.read_u8(status), 0);

    // MSI-X delivery should inject the programmed vector into the LAPIC.
    assert_eq!(pc.interrupts.borrow().get_pending(), Some(0x55));
}

#[test]
fn pc_platform_virtio_blk_msix_config_interrupt_triggers_lapic_vector() {
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            enable_ahci: false,
            enable_uhci: false,
            enable_virtio_blk: true,
            enable_virtio_msix: true,
            ..Default::default()
        },
    );
    let bdf = VIRTIO_BLK.bdf;

    // Switch into APIC mode so MSI delivery targets the LAPIC.
    pc.io.write_u8(IMCR_SELECT_PORT, IMCR_INDEX);
    pc.io.write_u8(IMCR_DATA_PORT, 0x01);
    assert_eq!(pc.interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    // Enable BAR0 MMIO decode + bus mastering, and disable legacy INTx so only MSI-X delivery is
    // observable.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0406);

    let bar0_base = read_bar0_base(&mut pc);
    assert_ne!(bar0_base, 0);
    assert_eq!(bar0_base % VIRTIO_BAR0_SIZE, 0);

    // Locate the MSI-X capability.
    let msix_cap = find_capability(&mut pc, bdf.bus, bdf.device, bdf.function, PCI_CAP_ID_MSIX)
        .expect("virtio-blk should expose MSI-X when enabled");

    // Enable MSI-X.
    let ctrl = read_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, msix_cap + 0x02);
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        msix_cap + 0x02,
        (ctrl & !(1 << 14)) | (1 << 15),
    );

    // Program table entry 1: destination = BSP (APIC ID 0), vector = 0x56.
    let table = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, msix_cap + 0x04);
    assert_eq!(table & 0x7, 0, "MSI-X table should live in BAR0 (BIR=0)");
    let table_offset = u64::from(table & !0x7);
    let entry1 = bar0_base + table_offset + 16;
    pc.memory.write_u32(entry1, 0xfee0_0000);
    pc.memory.write_u32(entry1 + 0x4, 0);
    pc.memory.write_u32(entry1 + 0x8, 0x0056);
    pc.memory.write_u32(entry1 + 0xc, 0); // unmasked

    // Assign MSI-X table entry 1 as the config interrupt vector.
    //
    // BAR0 layout for Aero's virtio-pci contract:
    // - common cfg @ 0x0000
    const COMMON: u64 = VIRTIO_COMMON_CFG_BAR0_OFFSET as u64;
    pc.memory.write_u16(bar0_base + COMMON + 0x10, 1); // msix_config_vector
    assert_eq!(
        pc.memory.read_u16(bar0_base + COMMON + 0x10),
        1,
        "msix_config_vector should be writable after MSI-X is enabled"
    );

    assert_eq!(pc.interrupts.borrow().get_pending(), None);

    // Trigger a device configuration interrupt directly from the virtio transport.
    pc.virtio_blk
        .as_ref()
        .unwrap()
        .borrow_mut()
        .signal_config_interrupt();

    assert_eq!(pc.interrupts.borrow().get_pending(), Some(0x56));
}

#[test]
fn pc_platform_virtio_blk_msix_unprogrammed_address_sets_pending_and_delivers_after_programming() {
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            enable_ahci: false,
            enable_uhci: false,
            enable_virtio_blk: true,
            enable_virtio_msix: true,
            ..Default::default()
        },
    );
    let bdf = VIRTIO_BLK.bdf;

    // Switch into APIC mode so MSI-X delivery targets the LAPIC.
    pc.io.write_u8(IMCR_SELECT_PORT, IMCR_INDEX);
    pc.io.write_u8(IMCR_DATA_PORT, 0x01);
    assert_eq!(pc.interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    // Enable BAR0 MMIO decode + bus mastering, and disable legacy INTx so MSI-X delivery is
    // required for interrupts to be observed.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0406);

    let bar0_base = read_bar0_base(&mut pc);
    assert_ne!(bar0_base, 0);
    assert_eq!(bar0_base % VIRTIO_BAR0_SIZE, 0);

    // Locate the MSI-X capability.
    let msix_cap = find_capability(&mut pc, bdf.bus, bdf.device, bdf.function, PCI_CAP_ID_MSIX)
        .expect("virtio-blk should expose an MSI-X capability when enabled");

    // Table/PBA offset/BIR must point into BAR0.
    let table = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, msix_cap + 0x04);
    let pba = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, msix_cap + 0x08);
    assert_eq!(table & 0x7, 0, "MSI-X table should live in BAR0 (BIR=0)");
    assert_eq!(pba & 0x7, 0, "MSI-X PBA should live in BAR0 (BIR=0)");
    let table_offset = u64::from(table & !0x7);
    let pba_offset = u64::from(pba & !0x7);

    // Enable MSI-X.
    let ctrl = read_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, msix_cap + 0x02);
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        msix_cap + 0x02,
        (ctrl & !(1 << 14)) | (1 << 15),
    );

    // Program table entry 0: vector = 0x57, but leave the address unprogrammed/invalid.
    let vector: u8 = 0x57;
    let entry0 = bar0_base + table_offset;
    pc.memory.write_u32(entry0, 0);
    pc.memory.write_u32(entry0 + 0x4, 0);
    pc.memory.write_u32(entry0 + 0x8, u32::from(vector));
    pc.memory.write_u32(entry0 + 0xc, 0); // unmasked

    // BAR0 layout for Aero's virtio-pci contract:
    // - common cfg @ 0x0000
    // - notify @ 0x1000
    // - ISR @ 0x2000 (read-to-clear)
    const COMMON: u64 = VIRTIO_COMMON_CFG_BAR0_OFFSET as u64;
    const NOTIFY: u64 = VIRTIO_NOTIFY_CFG_BAR0_OFFSET as u64;
    const ISR: u64 = VIRTIO_ISR_CFG_BAR0_OFFSET as u64;

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

    // Configure queue 0 and route its interrupts to MSI-X vector 0.
    const DESC_TABLE: u64 = 0x4000;
    const AVAIL_RING: u64 = 0x5000;
    const USED_RING: u64 = 0x6000;
    pc.memory.write_u16(bar0_base + COMMON + 0x16, 0); // queue_select
    let qsz = pc.memory.read_u16(bar0_base + COMMON + 0x18);
    assert!(qsz >= 2);

    // queue_msix_vector
    pc.memory.write_u16(bar0_base + COMMON + 0x1a, 0);
    assert_eq!(
        pc.memory.read_u16(bar0_base + COMMON + 0x1a),
        0,
        "queue_msix_vector should be writable after MSI-X is enabled"
    );

    pc.memory.write_u64(bar0_base + COMMON + 0x20, DESC_TABLE);
    pc.memory.write_u64(bar0_base + COMMON + 0x28, AVAIL_RING);
    pc.memory.write_u64(bar0_base + COMMON + 0x30, USED_RING);
    pc.memory.write_u16(bar0_base + COMMON + 0x1c, 1); // queue_enable

    // Build a minimal FLUSH request (no data buffers needed).
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

    assert_eq!(pc.interrupts.borrow().get_pending(), None);

    pc.memory.write_u16(bar0_base + NOTIFY, 0);
    pc.process_virtio_blk();

    assert_eq!(pc.memory.read_u16(USED_RING + 2), 1);
    assert_eq!(pc.memory.read_u8(status), 0);

    // Delivery is blocked by the invalid MSI-X table entry address; the vector should be latched as
    // pending in the MSI-X PBA without falling back to legacy INTx.
    assert!(
        !pc.virtio_blk.as_ref().unwrap().borrow().irq_level(),
        "virtio-blk should not assert legacy INTx while MSI-X is enabled"
    );
    assert_eq!(
        pc.interrupts.borrow().get_pending(),
        None,
        "expected no MSI-X delivery while the table entry address is invalid"
    );
    assert_ne!(
        pc.memory.read_u64(bar0_base + pba_offset) & 1,
        0,
        "expected MSI-X pending bit 0 to be set while the table entry address is invalid"
    );

    // Clear the virtio interrupt cause (ISR is read-to-clear). Pending MSI-X delivery should still
    // occur once MSI-X programming becomes valid, even without a new interrupt edge.
    let _isr = pc.memory.read_u8(bar0_base + ISR);
    assert_ne!(
        pc.memory.read_u64(bar0_base + pba_offset) & 1,
        0,
        "expected PBA pending bit to remain set after clearing the ISR"
    );
    assert_eq!(pc.interrupts.borrow().get_pending(), None);

    // Program a valid MSI-X message address; table writes service pending MSI-X vectors, so delivery
    // should occur without reasserting the interrupt condition.
    pc.memory.write_u32(entry0, 0xfee0_0000);
    assert_eq!(pc.interrupts.borrow().get_pending(), Some(vector));
    assert_eq!(
        pc.memory.read_u64(bar0_base + pba_offset) & 1,
        0,
        "expected MSI-X pending bit 0 to clear after delivery"
    );
}
