use aero_devices::pci::profile::VIRTIO_BLK;
use aero_pc_platform::PcPlatform;
use memory::MemoryBus as _;

fn cfg_addr(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | (offset as u32 & 0xFC)
}

fn read_cfg_u32(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    pc.io
        .write(0xCF8, 4, cfg_addr(bus, device, function, offset));
    pc.io.read(0xCFC, 4)
}

fn write_cfg_u16(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    pc.io
        .write(0xCF8, 4, cfg_addr(bus, device, function, offset));
    pc.io.write(0xCFC, 2, u32::from(value));
}

fn read_bar0_base(pc: &mut PcPlatform) -> u64 {
    let bdf = VIRTIO_BLK.bdf;
    let lo = read_cfg_u32(pc, bdf.bus, bdf.device, bdf.function, 0x10);
    let hi = read_cfg_u32(pc, bdf.bus, bdf.device, bdf.function, 0x14);
    (u64::from(hi) << 32) | u64::from(lo & 0xffff_fff0)
}

fn write_desc(pc: &mut PcPlatform, table: u64, index: u16, addr: u64, len: u32, flags: u16, next: u16) {
    let base = table + u64::from(index) * 16;
    pc.memory.write_u64(base, addr);
    pc.memory.write_u32(base + 8, len);
    pc.memory.write_u16(base + 12, flags);
    pc.memory.write_u16(base + 14, next);
}

#[test]
fn pc_platform_virtio_blk_processes_queue_and_raises_intx() {
    let mut pc = PcPlatform::new_with_virtio_blk(2 * 1024 * 1024);
    let bdf = VIRTIO_BLK.bdf;

    // Enumerate virtio-blk config space at the canonical BDF.
    let id = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x00);
    assert_eq!(id & 0xffff, u32::from(VIRTIO_BLK.vendor_id));
    assert_eq!((id >> 16) & 0xffff, u32::from(VIRTIO_BLK.device_id));

    let command = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04) as u16;
    assert_ne!(command & 0x2, 0, "BIOS POST should enable memory decoding");

    let bar0_base = read_bar0_base(&mut pc);
    assert_ne!(bar0_base, 0, "BAR0 should be assigned during BIOS POST");
    assert_eq!(bar0_base % 0x4000, 0, "BAR0 should be 0x4000-aligned");

    // Unmask IRQ2 (cascade) and IRQ11 so we can observe virtio-blk INTx via the legacy PIC.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        interrupts.pic_mut().set_masked(2, false);
        interrupts.pic_mut().set_masked(11, false);
    }

    // Keep bus mastering disabled initially so we can verify that:
    // - BAR0 notify writes do not perform DMA, and
    // - `process_virtio_blk()` is properly gated on PCI command.busmaster (bit 2).
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0002);

    // BAR0 layout for Aero's virtio-pci contract:
    // - common cfg @ 0x0000
    // - notify @ 0x1000, notify_off_multiplier = 4, queue0 notify_off = 0
    const COMMON: u64 = 0x0000;
    const NOTIFY: u64 = 0x1000;

    // Basic feature negotiation (accept whatever the device offers).
    pc.memory.write_u8(bar0_base + COMMON + 0x14, 1); // ACKNOWLEDGE
    pc.memory.write_u8(bar0_base + COMMON + 0x14, 1 | 2); // ACKNOWLEDGE | DRIVER

    // device_feature_select=0 -> read device_feature (low)
    pc.memory.write_u32(bar0_base + COMMON + 0x00, 0);
    let f0 = pc.memory.read_u32(bar0_base + COMMON + 0x04);
    pc.memory.write_u32(bar0_base + COMMON + 0x08, 0); // driver_feature_select=0
    pc.memory.write_u32(bar0_base + COMMON + 0x0c, f0);

    // device_feature_select=1 -> read high
    pc.memory.write_u32(bar0_base + COMMON + 0x00, 1);
    let f1 = pc.memory.read_u32(bar0_base + COMMON + 0x04);
    pc.memory.write_u32(bar0_base + COMMON + 0x08, 1);
    pc.memory.write_u32(bar0_base + COMMON + 0x0c, f1);

    pc.memory.write_u8(bar0_base + COMMON + 0x14, 1 | 2 | 8); // + FEATURES_OK
    pc.memory.write_u8(bar0_base + COMMON + 0x14, 1 | 2 | 8 | 4); // + DRIVER_OK

    // Configure queue 0.
    const DESC_TABLE: u64 = 0x4000;
    const AVAIL_RING: u64 = 0x5000;
    const USED_RING: u64 = 0x6000;
    pc.memory.write_u16(bar0_base + COMMON + 0x16, 0); // queue_select
    let _qsz = pc.memory.read_u16(bar0_base + COMMON + 0x18);
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

    write_desc(
        &mut pc,
        DESC_TABLE,
        0,
        header,
        16,
        VIRTQ_DESC_F_NEXT,
        1,
    );
    write_desc(&mut pc, DESC_TABLE, 1, status, 1, VIRTQ_DESC_F_WRITE, 0);

    // avail.flags=0, avail.idx=1, avail.ring[0]=0
    pc.memory.write_u16(AVAIL_RING, 0);
    pc.memory.write_u16(AVAIL_RING + 2, 1);
    pc.memory.write_u16(AVAIL_RING + 4, 0);
    pc.memory.write_u16(USED_RING, 0);
    pc.memory.write_u16(USED_RING + 2, 0);

    // Notify queue0 (offset 0 within notify region).
    pc.memory.write_u16(bar0_base + NOTIFY, 0);

    // Deferred-DMA check: notify must not cause queue processing in the MMIO handler.
    assert_eq!(pc.memory.read_u16(USED_RING + 2), 0);
    assert_eq!(pc.memory.read_u8(status), 0xff);

    // Bus-master gating check: processing must be a no-op until COMMAND.BME is set.
    pc.process_virtio_blk();
    assert_eq!(pc.memory.read_u16(USED_RING + 2), 0);
    assert_eq!(pc.memory.read_u8(status), 0xff);

    // Allow the device model to DMA from guest memory while processing.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

    pc.process_virtio_blk();

    // Used ring should advance and the status byte should be updated.
    assert_eq!(pc.memory.read_u16(USED_RING + 2), 1);
    assert_eq!(pc.memory.read_u8(status), 0);

    pc.poll_pci_intx_lines();
    let pending = pc
        .interrupts
        .borrow()
        .pic()
        .get_pending_vector()
        .expect("IRQ11 should be pending after processing virtio-blk");
    let irq = pc
        .interrupts
        .borrow()
        .pic()
        .vector_to_irq(pending)
        .expect("pending vector should decode to an IRQ number");
    assert_eq!(irq, 11);
}
