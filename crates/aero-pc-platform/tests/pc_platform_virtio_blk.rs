use aero_devices::pci::profile::{VIRTIO_BLK, VIRTIO_CAP_COMMON, VIRTIO_CAP_DEVICE, VIRTIO_CAP_ISR, VIRTIO_CAP_NOTIFY};
use aero_devices::pci::PciResourceAllocatorConfig;
use aero_io_snapshot::io::state::IoSnapshot;
use aero_interrupts::apic::IOAPIC_MMIO_BASE;
use aero_pc_platform::{PcPlatform, PcPlatformConfig};
use aero_platform::interrupts::{InterruptController, PlatformInterruptMode};
use aero_storage::VirtualDisk;
use aero_virtio::devices::VirtioDevice;
use aero_virtio::devices::blk::{BlockBackend, VirtioBlk, VIRTIO_BLK_SECTOR_SIZE};
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

fn read_cfg_u8(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8) -> u8 {
    let aligned = offset & 0xFC;
    let shift = (offset & 0x3) * 8;
    ((read_cfg_u32(pc, bus, device, function, aligned) >> shift) & 0xFF) as u8
}

fn write_cfg_u16(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    pc.io
        .write(0xCF8, 4, cfg_addr(bus, device, function, offset));
    pc.io.write(0xCFC, 2, u32::from(value));
}

fn write_cfg_u32(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    pc.io
        .write(0xCF8, 4, cfg_addr(bus, device, function, offset));
    pc.io.write(0xCFC, 4, value);
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

fn program_ioapic_entry(pc: &mut PcPlatform, gsi: u32, low: u32, high: u32) {
    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;
    pc.memory.write_u32(IOAPIC_MMIO_BASE, redtbl_low);
    pc.memory.write_u32(IOAPIC_MMIO_BASE + 0x10, low);
    pc.memory.write_u32(IOAPIC_MMIO_BASE, redtbl_high);
    pc.memory.write_u32(IOAPIC_MMIO_BASE + 0x10, high);
}

#[test]
fn pc_platform_sets_virtio_blk_intx_line_and_pin_registers() {
    let mut pc = PcPlatform::new_with_virtio_blk(2 * 1024 * 1024);
    let bdf = VIRTIO_BLK.bdf;

    let line = read_cfg_u8(&mut pc, bdf.bus, bdf.device, bdf.function, 0x3c);
    assert_eq!(line, 11, "00:09.0 INTA# should route to GSI/IRQ11 by default");

    let pin = read_cfg_u8(&mut pc, bdf.bus, bdf.device, bdf.function, 0x3d);
    assert_eq!(pin, 1, "Interrupt Pin should be INTA#");
}

#[test]
fn pc_platform_virtio_blk_bar0_is_mmio64_and_probes_return_size_mask() {
    let mut pc = PcPlatform::new_with_virtio_blk(2 * 1024 * 1024);
    let bdf = VIRTIO_BLK.bdf;

    // BAR0 is a 64-bit MMIO BAR.
    let bar0_lo = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x10);
    assert_eq!(bar0_lo & 0x7, 0x4);

    let bar0_orig_lo = bar0_lo;
    let bar0_orig_hi = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x14);

    // Probe BAR0 size mask.
    write_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x10, 0xFFFF_FFFF);
    write_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x14, 0xFFFF_FFFF);

    let bar0_probe_lo = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x10);
    let bar0_probe_hi = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x14);
    assert_eq!(
        bar0_probe_lo,
        0xFFFF_C004,
        "BAR0 probe should return size mask for 0x4000-byte MMIO64 BAR"
    );
    assert_eq!(bar0_probe_hi, 0xFFFF_FFFF);

    // Restore the BAR.
    write_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x10, bar0_orig_lo);
    write_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x14, bar0_orig_hi);
    assert_eq!(
        read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x10),
        bar0_orig_lo
    );
    assert_eq!(
        read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x14),
        bar0_orig_hi
    );
}

#[test]
fn pc_platform_gates_virtio_blk_mmio_on_pci_command_register() {
    let mut pc = PcPlatform::new_with_virtio_blk(2 * 1024 * 1024);
    let bdf = VIRTIO_BLK.bdf;

    let bar0_base = read_bar0_base(&mut pc);
    assert_ne!(bar0_base, 0);

    const COMMON: u64 = 0x0000;

    // device_status starts cleared.
    assert_eq!(pc.memory.read_u8(bar0_base + COMMON + 0x14), 0);

    // Disable PCI memory decoding: MMIO should behave like an unmapped region (reads return 0xFF).
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0000);
    assert_eq!(pc.memory.read_u32(bar0_base + COMMON + 0x04), 0xFFFF_FFFF);

    // Writes should be ignored while decoding is disabled.
    pc.memory.write_u8(bar0_base + COMMON + 0x14, 1);

    // Re-enable decoding: state should reflect that the write above did not reach the device.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0002);
    assert_eq!(pc.memory.read_u8(bar0_base + COMMON + 0x14), 0);

    // Now writes should take effect again.
    pc.memory.write_u8(bar0_base + COMMON + 0x14, 1);
    assert_eq!(pc.memory.read_u8(bar0_base + COMMON + 0x14), 1);
}

#[test]
fn pc_platform_routes_virtio_blk_mmio_after_bar0_reprogramming() {
    let mut pc = PcPlatform::new_with_virtio_blk(2 * 1024 * 1024);
    let bdf = VIRTIO_BLK.bdf;

    let bar0_base = read_bar0_base(&mut pc);
    assert_ne!(bar0_base, 0);

    const COMMON: u64 = 0x0000;

    // Touch device state at the original BAR0 base.
    pc.memory.write_u8(bar0_base + COMMON + 0x14, 1);
    assert_eq!(pc.memory.read_u8(bar0_base + COMMON + 0x14), 1);

    // Move BAR0 within the platform's PCI MMIO window.
    let alloc_cfg = PciResourceAllocatorConfig::default();
    let new_base = alloc_cfg.mmio_base + 0x00A0_0000;
    assert_eq!(new_base % 0x4000, 0);
    write_cfg_u32(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x10,
        new_base as u32,
    );

    // Old base should no longer decode.
    assert_eq!(pc.memory.read_u32(bar0_base + COMMON + 0x04), 0xFFFF_FFFF);

    // New base should decode and preserve state.
    assert_eq!(pc.memory.read_u8(new_base + COMMON + 0x14), 1);
}

#[test]
fn pc_platform_virtio_blk_device_cfg_reports_capacity_and_block_size() {
    let mut pc = PcPlatform::new_with_virtio_blk(2 * 1024 * 1024);

    let expected = {
        let virtio = pc.virtio_blk.as_ref().expect("virtio-blk enabled");
        let mut virtio = virtio.borrow_mut();
        let blk = virtio
            .device_as_any_mut()
            .downcast_mut::<VirtioBlk<Box<dyn VirtualDisk + Send>>>()
            .expect("virtio device should be VirtioBlk");
        let seg_max = u32::from(blk.queue_max_size(0).saturating_sub(2));
        let (capacity, blk_size) = {
            let backend = blk.backend_mut();
            (backend.len() / VIRTIO_BLK_SECTOR_SIZE, backend.blk_size())
        };
        (
            capacity,
            0u32, // size_max (contract v1: unused, must be 0)
            seg_max,
            blk_size,
        )
    };

    let bar0_base = read_bar0_base(&mut pc);
    assert_ne!(bar0_base, 0);

    // BAR0 layout for Aero's virtio-pci contract:
    // - device cfg @ 0x3000 (virtio-blk capacity, segment limits, block size, ...)
    const DEVICE_CFG: u64 = 0x3000;

    let capacity = pc.memory.read_u64(bar0_base + DEVICE_CFG);
    let size_max = pc.memory.read_u32(bar0_base + DEVICE_CFG + 8);
    let seg_max = pc.memory.read_u32(bar0_base + DEVICE_CFG + 12);
    let blk_size = pc.memory.read_u32(bar0_base + DEVICE_CFG + 20);

    assert_eq!(capacity, expected.0);
    assert_eq!(size_max, expected.1);
    assert_eq!(seg_max, expected.2);
    assert_eq!(blk_size, expected.3);
}

#[test]
fn pc_platform_virtio_blk_dma_writes_mark_dirty_pages_when_enabled() {
    let mut pc = PcPlatform::new_with_config_dirty_tracking(
        2 * 1024 * 1024,
        PcPlatformConfig {
            enable_virtio_blk: true,
            ..Default::default()
        },
        4096,
    );
    let bdf = VIRTIO_BLK.bdf;

    // Enable memory decoding + Bus Mastering so the device can DMA during processing.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

    let bar0_base = read_bar0_base(&mut pc);
    assert_ne!(bar0_base, 0);

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
    let qsz = pc.memory.read_u16(bar0_base + COMMON + 0x18);
    assert!(qsz >= 8);
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

    // Descriptor 0: header (read-only, NEXT=1).
    write_desc(&mut pc, DESC_TABLE, 0, header, 16, VIRTQ_DESC_F_NEXT, 1);
    // Descriptor 1: status (write-only).
    write_desc(&mut pc, DESC_TABLE, 1, status, 1, VIRTQ_DESC_F_WRITE, 0);

    pc.memory.write_u16(AVAIL_RING, 0);
    pc.memory.write_u16(AVAIL_RING + 2, 1);
    pc.memory.write_u16(AVAIL_RING + 4, 0);
    pc.memory.write_u16(USED_RING, 0);
    pc.memory.write_u16(USED_RING + 2, 0);

    // Doorbell via notify BAR offset.
    pc.memory.write_u16(bar0_base + NOTIFY, 0);

    // Clear dirty tracking for CPU-initiated setup writes. We want to observe only the DMA writes
    // performed by the device model (used ring + status byte update).
    pc.memory.clear_dirty();

    pc.process_virtio_blk();

    assert_eq!(pc.memory.read_u8(status), 0);
    assert_eq!(pc.memory.read_u16(USED_RING + 2), 1);

    let page_size = u64::from(pc.memory.dirty_page_size());
    let expected_used_page = USED_RING / page_size;
    let expected_status_page = status / page_size;

    let dirty = pc
        .memory
        .take_dirty_pages()
        .expect("dirty tracking enabled");
    assert!(
        dirty.contains(&expected_used_page),
        "dirty pages should include used ring page (got {dirty:?})"
    );
    assert!(
        dirty.contains(&expected_status_page),
        "dirty pages should include status byte page (got {dirty:?})"
    );
}

#[test]
fn pc_platform_virtio_blk_processes_queue_and_raises_intx() {
    let mut pc = PcPlatform::new_with_virtio_blk(2 * 1024 * 1024);
    let bdf = VIRTIO_BLK.bdf;

    // Enumerate virtio-blk config space at the canonical BDF.
    let id = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x00);
    assert_eq!(id & 0xffff, u32::from(VIRTIO_BLK.vendor_id));
    assert_eq!((id >> 16) & 0xffff, u32::from(VIRTIO_BLK.device_id));

    // Validate PCI identity matches the canonical `profile::VIRTIO_BLK` contract.
    let class_rev = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x08);
    let revision_id = (class_rev & 0xFF) as u8;
    let prog_if = ((class_rev >> 8) & 0xFF) as u8;
    let sub_class = ((class_rev >> 16) & 0xFF) as u8;
    let base_class = ((class_rev >> 24) & 0xFF) as u8;
    assert_eq!(revision_id, VIRTIO_BLK.revision_id);
    assert_eq!(base_class, VIRTIO_BLK.class.base_class);
    assert_eq!(sub_class, VIRTIO_BLK.class.sub_class);
    assert_eq!(prog_if, VIRTIO_BLK.class.prog_if);

    let subsystem = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x2C);
    assert_eq!(subsystem & 0xFFFF, u32::from(VIRTIO_BLK.subsystem_vendor_id));
    assert_eq!((subsystem >> 16) & 0xFFFF, u32::from(VIRTIO_BLK.subsystem_id));

    let header_bist = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x0C);
    let header_type = ((header_bist >> 16) & 0xFF) as u8;
    assert_eq!(header_type, VIRTIO_BLK.header_type);

    // Validate the vendor-specific capability list layout matches the virtio-pci contract.
    let mut caps: Vec<Vec<u8>> = Vec::new();
    let mut cap_ptr = read_cfg_u8(&mut pc, bdf.bus, bdf.device, bdf.function, 0x34);
    let mut guard = 0usize;
    while cap_ptr != 0 {
        guard += 1;
        assert!(guard <= 16, "capability list too long or cyclic");
        let cap_id = read_cfg_u8(&mut pc, bdf.bus, bdf.device, bdf.function, cap_ptr);
        assert_eq!(cap_id, 0x09, "unexpected capability ID at {cap_ptr:#x}");
        let next = read_cfg_u8(&mut pc, bdf.bus, bdf.device, bdf.function, cap_ptr.wrapping_add(1));
        let cap_len = read_cfg_u8(&mut pc, bdf.bus, bdf.device, bdf.function, cap_ptr.wrapping_add(2));
        assert!(cap_len >= 2, "invalid capability length");
        let payload_len = usize::from(cap_len - 2);
        let mut payload = vec![0u8; payload_len];
        for (i, b) in payload.iter_mut().enumerate() {
            let off = cap_ptr.wrapping_add(2).wrapping_add(i as u8);
            *b = read_cfg_u8(&mut pc, bdf.bus, bdf.device, bdf.function, off);
        }
        caps.push(payload);
        cap_ptr = next;
    }
    assert_eq!(caps.len(), 4);
    assert_eq!(caps[0].as_slice(), &VIRTIO_CAP_COMMON);
    assert_eq!(caps[1].as_slice(), &VIRTIO_CAP_NOTIFY);
    assert_eq!(caps[2].as_slice(), &VIRTIO_CAP_ISR);
    assert_eq!(caps[3].as_slice(), &VIRTIO_CAP_DEVICE);

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
    pc.memory.write_u32(bar0_base + COMMON, 0);
    let f0 = pc.memory.read_u32(bar0_base + COMMON + 0x04);
    pc.memory.write_u32(bar0_base + COMMON + 0x08, 0); // driver_feature_select=0
    pc.memory.write_u32(bar0_base + COMMON + 0x0c, f0);

    // device_feature_select=1 -> read high
    pc.memory.write_u32(bar0_base + COMMON, 1);
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

    // The virtio device should now be asserting its legacy INTx level until the guest reads the ISR.
    assert!(
        pc.virtio_blk
            .as_ref()
            .expect("virtio-blk enabled")
            .borrow()
            .irq_level(),
        "virtio device should assert INTx after completing a request"
    );

    // PCI command INTx Disable (bit 10) should suppress delivery even if the device is asserting
    // its legacy interrupt level.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0406);
    pc.poll_pci_intx_lines();
    assert!(
        pc.interrupts.borrow().pic().get_pending_vector().is_none(),
        "INTx disable should suppress interrupt delivery"
    );
    assert!(
        pc.virtio_blk
            .as_ref()
            .expect("virtio-blk enabled")
            .borrow()
            .irq_level(),
        "INTx disable should not change the device's asserted level"
    );

    // Re-enable INTx and ensure it is delivered to the PIC.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

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

    // Reading the ISR register should clear the interrupt level.
    let isr = pc.memory.read_u8(bar0_base + 0x2000);
    assert_ne!(isr, 0);
    assert!(
        !pc.virtio_blk
            .as_ref()
            .expect("virtio-blk enabled")
            .borrow()
            .irq_level()
    );
}

#[test]
fn pc_platform_routes_virtio_blk_intx_via_ioapic_in_apic_mode() {
    let mut pc = PcPlatform::new_with_virtio_blk(2 * 1024 * 1024);
    let bdf = VIRTIO_BLK.bdf;

    // Switch the platform into APIC mode via IMCR (0x22/0x23).
    pc.io.write_u8(0x22, 0x70);
    pc.io.write_u8(0x23, 0x01);
    assert_eq!(pc.interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    // Program IOAPIC entry for GSI11 to vector 0x61, level-triggered, active-low (default PCI INTx wiring).
    let vector = 0x61u32;
    let low = vector | (1 << 13) | (1 << 15); // polarity_low + level-triggered, unmasked
    program_ioapic_entry(&mut pc, 11, low, 0);

    // Enable memory decoding + Bus Mastering so the device can DMA during processing.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

    let bar0_base = read_bar0_base(&mut pc);

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

    write_desc(&mut pc, DESC_TABLE, 0, header, 16, VIRTQ_DESC_F_NEXT, 1);
    write_desc(&mut pc, DESC_TABLE, 1, status, 1, VIRTQ_DESC_F_WRITE, 0);

    pc.memory.write_u16(AVAIL_RING, 0);
    pc.memory.write_u16(AVAIL_RING + 2, 1);
    pc.memory.write_u16(AVAIL_RING + 4, 0);
    pc.memory.write_u16(USED_RING, 0);
    pc.memory.write_u16(USED_RING + 2, 0);

    pc.memory.write_u16(bar0_base + NOTIFY, 0);

    pc.process_virtio_blk();
    assert_eq!(pc.memory.read_u16(USED_RING + 2), 1);
    assert!(pc.virtio_blk.as_ref().unwrap().borrow().irq_level());

    pc.poll_pci_intx_lines();

    // IOAPIC should have delivered the vector through the LAPIC.
    assert_eq!(pc.interrupts.borrow().get_pending(), Some(vector as u8));

    // Simulate CPU taking the interrupt.
    pc.interrupts.borrow_mut().acknowledge(vector as u8);

    // Clear the device interrupt cause (ISR is read-to-clear).
    let _isr = pc.memory.read_u8(bar0_base + 0x2000);
    assert!(!pc.virtio_blk.as_ref().unwrap().borrow().irq_level());

    // Propagate the deasserted INTx level to the IOAPIC.
    pc.poll_pci_intx_lines();

    // End-of-interrupt should *not* cause a redelivery now that the line is deasserted.
    pc.interrupts.borrow_mut().eoi(vector as u8);
    assert_eq!(pc.interrupts.borrow().get_pending(), None);
}

#[test]
fn pc_platform_virtio_blk_snapshot_restore_preserves_virtqueue_progress() {
    let mut pc = PcPlatform::new_with_virtio_blk(2 * 1024 * 1024);
    let bdf = VIRTIO_BLK.bdf;

    // Enable memory decoding + Bus Mastering.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

    let bar0_base = read_bar0_base(&mut pc);
    assert_ne!(bar0_base, 0);
    assert_eq!(bar0_base % 0x4000, 0);

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
    let qsz = pc.memory.read_u16(bar0_base + COMMON + 0x18);
    assert!(qsz >= 8);
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

    // Descriptor 0: header (read-only, NEXT=1).
    write_desc(&mut pc, DESC_TABLE, 0, header, 16, VIRTQ_DESC_F_NEXT, 1);
    // Descriptor 1: status (write-only).
    write_desc(&mut pc, DESC_TABLE, 1, status, 1, VIRTQ_DESC_F_WRITE, 0);

    pc.memory.write_u16(AVAIL_RING, 0);
    pc.memory.write_u16(AVAIL_RING + 2, 1);
    pc.memory.write_u16(AVAIL_RING + 4, 0);
    pc.memory.write_u16(USED_RING, 0);
    pc.memory.write_u16(USED_RING + 2, 0);

    // Doorbell via notify BAR offset.
    pc.memory.write_u16(bar0_base + NOTIFY, 0);

    // Should not DMA/complete until explicitly processed.
    assert_eq!(pc.memory.read_u8(status), 0xff);
    assert_eq!(pc.memory.read_u16(USED_RING + 2), 0);

    pc.process_virtio_blk();

    assert_eq!(pc.memory.read_u8(status), 0);
    assert_eq!(pc.memory.read_u16(USED_RING + 2), 1);
    assert_eq!(
        pc.virtio_blk
            .as_ref()
            .unwrap()
            .borrow()
            .debug_queue_progress(0),
        Some((1, 1, false))
    );
    assert!(
        pc.virtio_blk.as_ref().unwrap().borrow().irq_level(),
        "virtio-blk INTx should be asserted after completion"
    );

    // Snapshot device + PCI config + guest RAM.
    let dev_snap = pc.virtio_blk.as_ref().unwrap().borrow().save_state();
    let pci_snap = pc.pci_cfg.borrow().save_state();

    let mut ram_img = vec![0u8; 2 * 1024 * 1024];
    pc.memory.read_physical(0, &mut ram_img);

    // Restore into a fresh platform.
    let mut restored = PcPlatform::new_with_virtio_blk(2 * 1024 * 1024);
    restored.memory.write_physical(0, &ram_img);
    restored.pci_cfg.borrow_mut().load_state(&pci_snap).unwrap();
    restored
        .virtio_blk
        .as_ref()
        .unwrap()
        .borrow_mut()
        .load_state(&dev_snap)
        .unwrap();

    // Verify progress state restored and no duplicate completion occurs.
    assert_eq!(restored.memory.read_u16(USED_RING + 2), 1);
    assert_eq!(
        restored
            .virtio_blk
            .as_ref()
            .unwrap()
            .borrow()
            .debug_queue_progress(0),
        Some((1, 1, false))
    );
    assert!(
        restored.virtio_blk.as_ref().unwrap().borrow().irq_level(),
        "restored virtio-blk should preserve asserted INTx level"
    );

    // Ring notify again without posting a new avail entry. This must not re-run the flush.
    let bar0_base2 = read_bar0_base(&mut restored);
    restored.memory.write_u16(bar0_base2 + NOTIFY, 0);
    restored.process_virtio_blk();
    assert_eq!(restored.memory.read_u16(USED_RING + 2), 1);

    // Post another FLUSH request at avail index 1.
    restored.memory.write_u8(status, 0xff);
    restored.memory.write_u16(AVAIL_RING + 6, 0);
    restored.memory.write_u16(AVAIL_RING + 2, 2);
    restored.memory.write_u16(bar0_base2 + NOTIFY, 0);
    restored.process_virtio_blk();

    assert_eq!(restored.memory.read_u8(status), 0);
    assert_eq!(restored.memory.read_u16(USED_RING + 2), 2);
    assert_eq!(
        restored
            .virtio_blk
            .as_ref()
            .unwrap()
            .borrow()
            .debug_queue_progress(0),
        Some((2, 2, false))
    );
}

#[test]
fn pc_platform_virtio_blk_snapshot_restore_processes_pending_request_without_renotify() {
    let mut pc = PcPlatform::new_with_virtio_blk(2 * 1024 * 1024);
    let bdf = VIRTIO_BLK.bdf;

    // Enable memory decoding + Bus Mastering.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);

    let bar0_base = read_bar0_base(&mut pc);
    assert_ne!(bar0_base, 0);
    assert_eq!(bar0_base % 0x4000, 0);

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
    let qsz = pc.memory.read_u16(bar0_base + COMMON + 0x18);
    assert!(qsz >= 8);
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

    // Descriptor 0: header (read-only, NEXT=1).
    write_desc(&mut pc, DESC_TABLE, 0, header, 16, VIRTQ_DESC_F_NEXT, 1);
    // Descriptor 1: status (write-only).
    write_desc(&mut pc, DESC_TABLE, 1, status, 1, VIRTQ_DESC_F_WRITE, 0);

    pc.memory.write_u16(AVAIL_RING, 0);
    pc.memory.write_u16(AVAIL_RING + 2, 1);
    pc.memory.write_u16(AVAIL_RING + 4, 0);
    pc.memory.write_u16(USED_RING, 0);
    pc.memory.write_u16(USED_RING + 2, 0);

    // Guest "kicks" the queue but we snapshot before the platform processes it.
    pc.memory.write_u16(bar0_base + NOTIFY, 0);
    assert_eq!(pc.memory.read_u8(status), 0xff);
    assert_eq!(pc.memory.read_u16(USED_RING + 2), 0);

    let dev_snap = pc.virtio_blk.as_ref().unwrap().borrow().save_state();
    let pci_snap = pc.pci_cfg.borrow().save_state();
    let mut ram_img = vec![0u8; 2 * 1024 * 1024];
    pc.memory.read_physical(0, &mut ram_img);

    // Restore into a fresh platform.
    let mut restored = PcPlatform::new_with_virtio_blk(2 * 1024 * 1024);
    restored.memory.write_physical(0, &ram_img);
    restored.pci_cfg.borrow_mut().load_state(&pci_snap).unwrap();
    restored
        .virtio_blk
        .as_ref()
        .unwrap()
        .borrow_mut()
        .load_state(&dev_snap)
        .unwrap();

    // Without re-notifying, the platform should still be able to process the pending avail entry.
    restored.process_virtio_blk();
    assert_eq!(restored.memory.read_u8(status), 0);
    assert_eq!(restored.memory.read_u16(USED_RING + 2), 1);
}
