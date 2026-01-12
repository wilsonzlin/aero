use aero_virtio::devices::blk::{
    BlockBackend, BlockBackendError, VirtioBlk, VIRTIO_BLK_S_IOERR, VIRTIO_BLK_S_UNSUPP,
    VIRTIO_BLK_T_FLUSH, VIRTIO_BLK_T_IN, VIRTIO_BLK_T_OUT,
};
use aero_virtio::memory::{
    read_u32_le, write_u16_le, write_u32_le, write_u64_le, GuestMemory, GuestRam,
};
use aero_virtio::pci::{
    InterruptLog, InterruptSink, VirtioPciDevice, PCI_VENDOR_ID_VIRTIO, VIRTIO_PCI_CAP_COMMON_CFG,
    VIRTIO_PCI_CAP_DEVICE_CFG, VIRTIO_PCI_CAP_ISR_CFG, VIRTIO_PCI_CAP_NOTIFY_CFG,
    VIRTIO_STATUS_ACKNOWLEDGE, VIRTIO_STATUS_DRIVER, VIRTIO_STATUS_DRIVER_OK,
    VIRTIO_STATUS_FEATURES_OK,
};
use aero_storage::{DiskError as StorageDiskError, MemBackend, RawDisk, VirtualDisk};
use aero_io_snapshot::io::state::IoSnapshot;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[derive(Clone)]
struct SharedDisk {
    data: Rc<RefCell<Vec<u8>>>,
    flushes: Rc<Cell<u32>>,
}

impl BlockBackend for SharedDisk {
    fn len(&self) -> u64 {
        self.data.borrow().len() as u64
    }

    fn read_at(&mut self, offset: u64, dst: &mut [u8]) -> Result<(), BlockBackendError> {
        let offset: usize = offset
            .try_into()
            .map_err(|_| BlockBackendError::OutOfBounds)?;
        let end = offset
            .checked_add(dst.len())
            .ok_or(BlockBackendError::OutOfBounds)?;
        dst.copy_from_slice(&self.data.borrow()[offset..end]);
        Ok(())
    }

    fn write_at(&mut self, offset: u64, src: &[u8]) -> Result<(), BlockBackendError> {
        let offset: usize = offset
            .try_into()
            .map_err(|_| BlockBackendError::OutOfBounds)?;
        let end = offset
            .checked_add(src.len())
            .ok_or(BlockBackendError::OutOfBounds)?;
        self.data.borrow_mut()[offset..end].copy_from_slice(src);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), BlockBackendError> {
        self.flushes.set(self.flushes.get().saturating_add(1));
        Ok(())
    }

    fn device_id(&self) -> [u8; 20] {
        *b"aero-virtio-testdisk"
    }
}

#[derive(Default)]
struct Caps {
    common: u64,
    notify: u64,
    isr: u64,
    device: u64,
    notify_mult: u32,
}

fn parse_caps(dev: &mut VirtioPciDevice) -> Caps {
    let mut cfg = [0u8; 256];
    dev.config_read(0, &mut cfg);
    let mut caps = Caps::default();

    let mut ptr = cfg[0x34] as usize;
    while ptr != 0 {
        assert_eq!(cfg[ptr], 0x09);
        let next = cfg[ptr + 1] as usize;
        let cap_len = cfg[ptr + 2] as usize;
        let cfg_type = cfg[ptr + 3];
        let offset = u32::from_le_bytes(cfg[ptr + 8..ptr + 12].try_into().unwrap()) as u64;
        match cfg_type {
            VIRTIO_PCI_CAP_COMMON_CFG => caps.common = offset,
            VIRTIO_PCI_CAP_NOTIFY_CFG => {
                caps.notify = offset;
                caps.notify_mult = u32::from_le_bytes(cfg[ptr + 16..ptr + 20].try_into().unwrap());
            }
            VIRTIO_PCI_CAP_ISR_CFG => caps.isr = offset,
            VIRTIO_PCI_CAP_DEVICE_CFG => caps.device = offset,
            _ => {}
        }
        assert!(cap_len >= 16);
        ptr = next;
    }

    caps
}

fn bar_read_u32(dev: &mut VirtioPciDevice, off: u64) -> u32 {
    let mut buf = [0u8; 4];
    dev.bar0_read(off, &mut buf);
    u32::from_le_bytes(buf)
}

fn bar_read_u16(dev: &mut VirtioPciDevice, off: u64) -> u16 {
    let mut buf = [0u8; 2];
    dev.bar0_read(off, &mut buf);
    u16::from_le_bytes(buf)
}

fn bar_read_u64(dev: &mut VirtioPciDevice, off: u64) -> u64 {
    let mut buf = [0u8; 8];
    dev.bar0_read(off, &mut buf);
    u64::from_le_bytes(buf)
}

fn bar_write_u32(dev: &mut VirtioPciDevice, off: u64, val: u32) {
    dev.bar0_write(off, &val.to_le_bytes());
}

fn bar_write_u16(dev: &mut VirtioPciDevice, off: u64, val: u16) {
    dev.bar0_write(off, &val.to_le_bytes());
}

fn bar_write_u64(dev: &mut VirtioPciDevice, off: u64, val: u64) {
    dev.bar0_write(off, &val.to_le_bytes());
}

fn bar_write_u8(dev: &mut VirtioPciDevice, off: u64, val: u8) {
    dev.bar0_write(off, &[val]);
}

fn write_desc(
    mem: &mut GuestRam,
    table: u64,
    index: u16,
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
) {
    let base = table + u64::from(index) * 16;
    write_u64_le(mem, base, addr).unwrap();
    write_u32_le(mem, base + 8, len).unwrap();
    write_u16_le(mem, base + 12, flags).unwrap();
    write_u16_le(mem, base + 14, next).unwrap();
}

const DESC_TABLE: u64 = 0x4000;
const AVAIL_RING: u64 = 0x5000;
const USED_RING: u64 = 0x6000;

type Setup = (
    VirtioPciDevice,
    Caps,
    GuestRam,
    Rc<RefCell<Vec<u8>>>,
    Rc<Cell<u32>>,
);

#[derive(Clone, Default)]
struct TestIrq {
    legacy_count: Rc<Cell<u64>>,
    legacy_level: Rc<Cell<bool>>,
}

impl TestIrq {
    fn legacy_count(&self) -> u64 {
        self.legacy_count.get()
    }

    fn legacy_level(&self) -> bool {
        self.legacy_level.get()
    }
}

impl InterruptSink for TestIrq {
    fn raise_legacy_irq(&mut self) {
        self.legacy_level.set(true);
        self.legacy_count.set(self.legacy_count.get().saturating_add(1));
    }

    fn lower_legacy_irq(&mut self) {
        self.legacy_level.set(false);
    }

    fn signal_msix(&mut self, _vector: u16) {
        // Not exercised by these tests.
    }
}

type SetupWithIrq = (
    VirtioPciDevice,
    Caps,
    GuestRam,
    Rc<RefCell<Vec<u8>>>,
    Rc<Cell<u32>>,
    TestIrq,
);

fn setup_pci_device(mut dev: VirtioPciDevice) -> (VirtioPciDevice, Caps, GuestRam) {
    // Basic PCI identification.
    let mut id = [0u8; 4];
    dev.config_read(0, &mut id);
    let vendor = u16::from_le_bytes(id[0..2].try_into().unwrap());
    assert_eq!(vendor, PCI_VENDOR_ID_VIRTIO);

    // BAR0 size probing (basic PCI correctness).
    dev.config_write(0x10, &0xffff_ffffu32.to_le_bytes());
    dev.config_write(0x14, &0xffff_ffffu32.to_le_bytes());
    let mut bar = [0u8; 4];
    dev.config_read(0x10, &mut bar);
    let expected_mask = ((!(dev.bar0_size() as u32 - 1)) & 0xffff_fff0) | 0x4;
    assert_eq!(u32::from_le_bytes(bar), expected_mask);
    dev.config_read(0x14, &mut bar);
    assert_eq!(u32::from_le_bytes(bar), 0xffff_ffff);
    dev.config_write(0x10, &0x8000_0000u32.to_le_bytes());
    dev.config_write(0x14, &0u32.to_le_bytes());
    dev.config_read(0x10, &mut bar);
    assert_eq!(u32::from_le_bytes(bar), 0x8000_0004);
    dev.config_read(0x14, &mut bar);
    assert_eq!(u32::from_le_bytes(bar), 0);

    let caps = parse_caps(&mut dev);
    // `common` may legitimately be at BAR offset 0; the rest should be mapped.
    assert_ne!(caps.notify, 0);
    assert_ne!(caps.isr, 0);
    assert_ne!(caps.device, 0);
    assert_ne!(caps.notify_mult, 0);

    let mem = GuestRam::new(0x10000);

    // Feature negotiation.
    bar_write_u8(&mut dev, caps.common + 0x14, VIRTIO_STATUS_ACKNOWLEDGE);
    bar_write_u8(
        &mut dev,
        caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
    );

    bar_write_u32(&mut dev, caps.common, 0); // device_feature_select
    let f0 = bar_read_u32(&mut dev, caps.common + 0x04);
    bar_write_u32(&mut dev, caps.common + 0x08, 0); // driver_feature_select
    bar_write_u32(&mut dev, caps.common + 0x0c, f0);

    bar_write_u32(&mut dev, caps.common, 1);
    let f1 = bar_read_u32(&mut dev, caps.common + 0x04);
    bar_write_u32(&mut dev, caps.common + 0x08, 1);
    bar_write_u32(&mut dev, caps.common + 0x0c, f1);

    bar_write_u8(
        &mut dev,
        caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK,
    );
    bar_write_u8(
        &mut dev,
        caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE
            | VIRTIO_STATUS_DRIVER
            | VIRTIO_STATUS_FEATURES_OK
            | VIRTIO_STATUS_DRIVER_OK,
    );

    // Configure queue 0.
    bar_write_u16(&mut dev, caps.common + 0x16, 0); // queue_select
    let qsz = bar_read_u16(&mut dev, caps.common + 0x18);
    assert!(qsz >= 8);

    bar_write_u64(&mut dev, caps.common + 0x20, DESC_TABLE);
    bar_write_u64(&mut dev, caps.common + 0x28, AVAIL_RING);
    bar_write_u64(&mut dev, caps.common + 0x30, USED_RING);
    bar_write_u16(&mut dev, caps.common + 0x1c, 1); // queue_enable

    (dev, caps, mem)
}

fn setup() -> Setup {
    let backing = Rc::new(RefCell::new(vec![0u8; 4096]));
    let flushes = Rc::new(Cell::new(0u32));
    let backend = SharedDisk {
        data: backing.clone(),
        flushes: flushes.clone(),
    };

    let blk = VirtioBlk::new(backend);
    let dev = VirtioPciDevice::new(Box::new(blk), Box::new(InterruptLog::default()));

    let (dev, caps, mem) = setup_pci_device(dev);

    (dev, caps, mem, backing, flushes)
}

fn setup_with_irq(irq: TestIrq) -> SetupWithIrq {
    let backing = Rc::new(RefCell::new(vec![0u8; 4096]));
    let flushes = Rc::new(Cell::new(0u32));
    let backend = SharedDisk {
        data: backing.clone(),
        flushes: flushes.clone(),
    };

    let blk = VirtioBlk::new(backend);
    let dev = VirtioPciDevice::new(Box::new(blk), Box::new(irq.clone()));

    let (dev, caps, mem) = setup_pci_device(dev);

    (dev, caps, mem, backing, flushes, irq)
}

fn kick_queue0(dev: &mut VirtioPciDevice, caps: &Caps, mem: &mut GuestRam) {
    dev.bar0_write(caps.notify, &0u16.to_le_bytes());
    dev.process_notified_queues(mem);
}

fn setup_aero_storage_disk() -> (VirtioPciDevice, Caps, GuestRam) {
    let disk = RawDisk::create(MemBackend::new(), 4096).unwrap();
    let backend: Box<dyn VirtualDisk + Send> = Box::new(disk);
    let blk = VirtioBlk::new(backend);
    let dev = VirtioPciDevice::new(Box::new(blk), Box::new(InterruptLog::default()));
    setup_pci_device(dev)
}

#[test]
fn virtio_blk_config_exposes_capacity_and_block_size() {
    let (mut dev, caps, _mem, _backing, _flushes) = setup();

    // virtio-blk config: capacity in 512-byte sectors.
    let cap = bar_read_u64(&mut dev, caps.device);
    assert_eq!(cap, 8);

    let size_max = bar_read_u32(&mut dev, caps.device + 8);
    let seg_max = bar_read_u32(&mut dev, caps.device + 12);
    let blk_size = bar_read_u32(&mut dev, caps.device + 20);

    assert_eq!(size_max, 0);
    assert_eq!(seg_max, 126);
    assert_eq!(blk_size, 512);
}

#[test]
fn virtio_blk_config_read_out_of_range_offsets_return_zeroes() {
    let cfg = aero_virtio::devices::blk::VirtioBlkConfig {
        capacity: 8,
        size_max: 0,
        seg_max: 126,
        blk_size: 512,
    };

    let mut buf = [0xa5u8; 16];
    cfg.read(u64::MAX, &mut buf);
    assert!(buf.iter().all(|&b| b == 0));
}

#[cfg(target_pointer_width = "32")]
#[test]
fn virtio_blk_config_read_does_not_truncate_large_offsets_on_32bit() {
    let cfg = aero_virtio::devices::blk::VirtioBlkConfig {
        capacity: 8,
        size_max: 0,
        seg_max: 126,
        blk_size: 512,
    };

    // On 32-bit targets, `u64 as usize` truncates. Ensure out-of-range offsets never alias within
    // the config struct.
    let big_offset = u64::from(u32::MAX) + 1;
    let mut buf = [0xa5u8; 8];
    cfg.read(big_offset, &mut buf);
    assert!(buf.iter().all(|&b| b == 0));
}

#[test]
fn virtio_blk_processes_multi_segment_write_then_read() {
    let (mut dev, caps, mut mem, backing, _flushes) = setup();

    // Write request: OUT sector 1 split across two data descriptors.
    let header = 0x7000;
    let data = 0x8000;
    let data_b = 0x8200;
    let status = 0x9000;

    let sector = 1u64;
    write_u32_le(&mut mem, header, VIRTIO_BLK_T_OUT).unwrap();
    write_u32_le(&mut mem, header + 4, 0).unwrap();
    write_u64_le(&mut mem, header + 8, sector).unwrap();

    let payload: Vec<u8> = (0..512u16).flat_map(|v| v.to_le_bytes()).collect();
    let (a, b) = payload.split_at(512);
    mem.write(data, a).unwrap();
    mem.write(data_b, b).unwrap();
    mem.write(status, &[0xff]).unwrap();

    write_desc(&mut mem, DESC_TABLE, 0, header, 16, 0x0001, 1);
    write_desc(&mut mem, DESC_TABLE, 1, data, 512, 0x0001, 2);
    write_desc(&mut mem, DESC_TABLE, 2, data_b, 512, 0x0001, 3);
    write_desc(&mut mem, DESC_TABLE, 3, status, 1, 0x0002, 0);

    write_u16_le(&mut mem, AVAIL_RING, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 1).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 4, 0).unwrap();

    write_u16_le(&mut mem, USED_RING, 0).unwrap();
    write_u16_le(&mut mem, USED_RING + 2, 0).unwrap();

    kick_queue0(&mut dev, &caps, &mut mem);

    assert_eq!(
        &backing.borrow()[(sector * 512) as usize..(sector * 512) as usize + payload.len()],
        payload.as_slice()
    );
    assert_eq!(mem.get_slice(status, 1).unwrap()[0], 0);
    // Contract v1: used.len MUST be 0 for all virtio-blk completions.
    assert_eq!(read_u32_le(&mem, USED_RING + 8).unwrap(), 0);

    // Read request: IN sector 1 into two write-only buffers.
    let data2 = 0xA000;
    let data2_b = 0xA200;
    mem.write(data2, &vec![0u8; payload.len()]).unwrap();
    mem.write(status, &[0xff]).unwrap();

    write_u32_le(&mut mem, header, VIRTIO_BLK_T_IN).unwrap();
    write_u64_le(&mut mem, header + 8, sector).unwrap();

    write_desc(&mut mem, DESC_TABLE, 0, header, 16, 0x0001, 1);
    write_desc(&mut mem, DESC_TABLE, 1, data2, 512, 0x0001 | 0x0002, 2);
    write_desc(&mut mem, DESC_TABLE, 2, data2_b, 512, 0x0001 | 0x0002, 3);
    write_desc(&mut mem, DESC_TABLE, 3, status, 1, 0x0002, 0);

    // Add to avail ring at index 1.
    write_u16_le(&mut mem, AVAIL_RING + 4 + 2, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 2).unwrap();

    kick_queue0(&mut dev, &caps, &mut mem);

    let got = mem.get_slice(data2, payload.len()).unwrap();
    assert_eq!(got, payload.as_slice());
    assert_eq!(mem.get_slice(status, 1).unwrap()[0], 0);
    assert_eq!(read_u32_le(&mem, USED_RING + 16).unwrap(), 0);

    // FLUSH request.
    mem.write(status, &[0xff]).unwrap();
    write_u32_le(&mut mem, header, VIRTIO_BLK_T_FLUSH).unwrap();
    write_u64_le(&mut mem, header + 8, 0).unwrap();
    write_desc(&mut mem, DESC_TABLE, 0, header, 16, 0x0001, 1);
    write_desc(&mut mem, DESC_TABLE, 1, status, 1, 0x0002, 0);
    write_u16_le(&mut mem, AVAIL_RING + 4 + 4, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 3).unwrap();
    kick_queue0(&mut dev, &caps, &mut mem);
    assert_eq!(mem.get_slice(status, 1).unwrap()[0], 0);
    assert_eq!(read_u32_le(&mem, USED_RING + 4 + 2 * 8 + 4).unwrap(), 0);

    // Unsupported request type should return UNSUPP.
    let id_buf = 0xB000;
    mem.write(id_buf, &[0u8; 20]).unwrap();
    mem.write(status, &[0xff]).unwrap();
    write_u32_le(&mut mem, header, 8).unwrap();
    write_u64_le(&mut mem, header + 8, 0).unwrap();
    write_desc(&mut mem, DESC_TABLE, 0, header, 16, 0x0001, 1);
    write_desc(&mut mem, DESC_TABLE, 1, id_buf, 20, 0x0001 | 0x0002, 2);
    write_desc(&mut mem, DESC_TABLE, 2, status, 1, 0x0002, 0);
    write_u16_le(&mut mem, AVAIL_RING + 4 + 6, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 4).unwrap();
    kick_queue0(&mut dev, &caps, &mut mem);
    assert_eq!(mem.get_slice(status, 1).unwrap()[0], VIRTIO_BLK_S_UNSUPP);
    assert_eq!(read_u32_le(&mem, USED_RING + 4 + 3 * 8 + 4).unwrap(), 0);
}

#[test]
fn virtio_blk_flush_calls_backend_flush() {
    let (mut dev, caps, mut mem, _backing, flushes) = setup();

    let header = 0x7000;
    let status = 0x9000;

    write_u32_le(&mut mem, header, VIRTIO_BLK_T_FLUSH).unwrap();
    write_u32_le(&mut mem, header + 4, 0).unwrap();
    write_u64_le(&mut mem, header + 8, 0).unwrap();
    mem.write(status, &[0xff]).unwrap();

    write_desc(&mut mem, DESC_TABLE, 0, header, 16, 0x0001, 1);
    write_desc(&mut mem, DESC_TABLE, 1, status, 1, 0x0002, 0);

    write_u16_le(&mut mem, AVAIL_RING, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 1).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 4, 0).unwrap();

    write_u16_le(&mut mem, USED_RING, 0).unwrap();
    write_u16_le(&mut mem, USED_RING + 2, 0).unwrap();

    kick_queue0(&mut dev, &caps, &mut mem);

    assert_eq!(mem.get_slice(status, 1).unwrap()[0], 0);
    assert_eq!(flushes.get(), 1);
    assert_eq!(read_u32_le(&mem, USED_RING + 8).unwrap(), 0);
}

#[test]
fn virtio_blk_snapshot_restore_preserves_virtqueue_progress_and_does_not_duplicate_requests() {
    let irq0 = TestIrq::default();
    let (mut dev, caps, mut mem, backing, flushes, irq0) = setup_with_irq(irq0);

    // Build a FLUSH request so duplicate processing is observable via `flushes`.
    let header = 0x7000;
    let status = 0x9000;

    write_u32_le(&mut mem, header, VIRTIO_BLK_T_FLUSH).unwrap();
    write_u32_le(&mut mem, header + 4, 0).unwrap();
    write_u64_le(&mut mem, header + 8, 0).unwrap();
    mem.write(status, &[0xff]).unwrap();

    write_desc(&mut mem, DESC_TABLE, 0, header, 16, 0x0001, 1);
    write_desc(&mut mem, DESC_TABLE, 1, status, 1, 0x0002, 0);

    write_u16_le(&mut mem, AVAIL_RING, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 1).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 4, 0).unwrap();

    write_u16_le(&mut mem, USED_RING, 0).unwrap();
    write_u16_le(&mut mem, USED_RING + 2, 0).unwrap();

    kick_queue0(&mut dev, &caps, &mut mem);

    assert_eq!(mem.get_slice(status, 1).unwrap()[0], 0);
    assert_eq!(flushes.get(), 1);
    assert_eq!(dev.debug_queue_used_idx(&mem, 0), Some(1));
    assert_eq!(dev.debug_queue_progress(0), Some((1, 1, false)));
    assert_eq!(irq0.legacy_count(), 1);
    assert!(irq0.legacy_level(), "legacy irq should be asserted after flush completion");

    // Snapshot the device + guest memory image after completion.
    let snap_bytes = dev.save_state();
    let mem_snap = mem.clone();

    // Restore into a fresh device instance (same disk backend, same guest memory image).
    let irq1 = TestIrq::default();
    let backend = SharedDisk {
        data: backing.clone(),
        flushes: flushes.clone(),
    };
    let blk = VirtioBlk::new(backend);
    let mut restored = VirtioPciDevice::new(Box::new(blk), Box::new(irq1.clone()));
    restored.load_state(&snap_bytes).unwrap();
    let mut mem2 = mem_snap.clone();
    let caps_restored = parse_caps(&mut restored);

    // Ensure virtqueue progress is preserved.
    assert_eq!(restored.debug_queue_used_idx(&mem2, 0), Some(1));
    assert_eq!(restored.debug_queue_progress(0), Some((1, 1, false)));

    // Kicking the queue without adding new avail entries must not re-run the request.
    let irq_before = irq1.legacy_count();
    kick_queue0(&mut restored, &caps_restored, &mut mem2);
    assert_eq!(flushes.get(), 1, "duplicate FLUSH should not be executed after restore");
    assert_eq!(restored.debug_queue_used_idx(&mem2, 0), Some(1));
    assert_eq!(irq1.legacy_count(), irq_before);

    // Add another FLUSH request and ensure it still completes post-restore.
    mem2.write(status, &[0xff]).unwrap();
    write_u16_le(&mut mem2, AVAIL_RING + 4 + 2, 0).unwrap(); // ring[1] = head 0
    write_u16_le(&mut mem2, AVAIL_RING + 2, 2).unwrap(); // idx = 2

    kick_queue0(&mut restored, &caps_restored, &mut mem2);
    assert_eq!(mem2.get_slice(status, 1).unwrap()[0], 0);
    assert_eq!(flushes.get(), 2);
    assert_eq!(restored.debug_queue_used_idx(&mem2, 0), Some(2));
    assert_eq!(restored.debug_queue_progress(0), Some((2, 2, false)));
}

#[test]
fn virtio_blk_snapshot_restore_preserves_pending_avail_entries_without_renotify() {
    let irq0 = TestIrq::default();
    let (mut dev, caps, mut mem, backing, flushes, _irq0) = setup_with_irq(irq0);

    // Build a FLUSH request, but snapshot after the guest "kicks" the queue and before the
    // platform processes the notify.
    let header = 0x7000;
    let status = 0x9000;

    write_u32_le(&mut mem, header, VIRTIO_BLK_T_FLUSH).unwrap();
    write_u32_le(&mut mem, header + 4, 0).unwrap();
    write_u64_le(&mut mem, header + 8, 0).unwrap();
    mem.write(status, &[0xff]).unwrap();

    write_desc(&mut mem, DESC_TABLE, 0, header, 16, 0x0001, 1);
    write_desc(&mut mem, DESC_TABLE, 1, status, 1, 0x0002, 0);

    write_u16_le(&mut mem, AVAIL_RING, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 1).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 4, 0).unwrap();
    write_u16_le(&mut mem, USED_RING, 0).unwrap();
    write_u16_le(&mut mem, USED_RING + 2, 0).unwrap();

    // Guest writes the notify register, but the platform has not yet called
    // `process_notified_queues()`.
    dev.bar0_write(caps.notify, &0u16.to_le_bytes());

    assert_eq!(mem.get_slice(status, 1).unwrap()[0], 0xff);
    assert_eq!(flushes.get(), 0);
    assert_eq!(dev.debug_queue_used_idx(&mem, 0), Some(0));

    let snap_bytes = dev.save_state();
    let mem_snap = mem.clone();

    // Restore into a fresh device instance with the same disk backend and guest memory image.
    let irq1 = TestIrq::default();
    let backend = SharedDisk {
        data: backing.clone(),
        flushes: flushes.clone(),
    };
    let blk = VirtioBlk::new(backend);
    let mut restored = VirtioPciDevice::new(Box::new(blk), Box::new(irq1));
    restored.load_state(&snap_bytes).unwrap();

    let mut mem2 = mem_snap.clone();

    // The platform should not need to re-notify after restore: the device can detect that
    // `avail.idx != next_avail` and process the pending entry.
    restored.process_notified_queues(&mut mem2);

    assert_eq!(mem2.get_slice(status, 1).unwrap()[0], 0);
    assert_eq!(flushes.get(), 1);
    assert_eq!(restored.debug_queue_used_idx(&mem2, 0), Some(1));
    assert_eq!(restored.debug_queue_progress(0), Some((1, 1, false)));
}

#[test]
fn malformed_chains_return_ioerr_without_panicking() {
    let (mut dev, caps, mut mem, _backing, _flushes) = setup();

    // OUT request where the data descriptor is incorrectly marked write-only.
    let header = 0x7000;
    let data = 0x8000;
    let status = 0x9000;

    write_u32_le(&mut mem, header, VIRTIO_BLK_T_OUT).unwrap();
    write_u32_le(&mut mem, header + 4, 0).unwrap();
    write_u64_le(&mut mem, header + 8, 1).unwrap();
    mem.write(data, &vec![0xa5u8; 512]).unwrap();
    mem.write(status, &[0xff]).unwrap();

    write_desc(&mut mem, DESC_TABLE, 0, header, 16, 0x0001, 1);
    write_desc(&mut mem, DESC_TABLE, 1, data, 512, 0x0001 | 0x0002, 2);
    write_desc(&mut mem, DESC_TABLE, 2, status, 1, 0x0002, 0);

    write_u16_le(&mut mem, AVAIL_RING, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 1).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 4, 0).unwrap();

    write_u16_le(&mut mem, USED_RING, 0).unwrap();
    write_u16_le(&mut mem, USED_RING + 2, 0).unwrap();

    kick_queue0(&mut dev, &caps, &mut mem);

    assert_eq!(mem.get_slice(status, 1).unwrap()[0], 1);
    assert_eq!(read_u32_le(&mem, USED_RING + 8).unwrap(), 0);
}

#[test]
fn virtio_blk_rejects_non_sector_multiple_requests() {
    let (mut dev, caps, mut mem, backing, _flushes) = setup();

    let header = 0x7000;
    let data = 0x8000;
    let status = 0x9000;

    write_u32_le(&mut mem, header, VIRTIO_BLK_T_OUT).unwrap();
    write_u32_le(&mut mem, header + 4, 0).unwrap();
    write_u64_le(&mut mem, header + 8, 0).unwrap();

    mem.write(data, &vec![0xa5u8; 513]).unwrap();
    mem.write(status, &[0xff]).unwrap();

    write_desc(&mut mem, DESC_TABLE, 0, header, 16, 0x0001, 1);
    write_desc(&mut mem, DESC_TABLE, 1, data, 513, 0x0001, 2);
    write_desc(&mut mem, DESC_TABLE, 2, status, 1, 0x0002, 0);

    write_u16_le(&mut mem, AVAIL_RING, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 1).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 4, 0).unwrap();
    write_u16_le(&mut mem, USED_RING, 0).unwrap();
    write_u16_le(&mut mem, USED_RING + 2, 0).unwrap();

    kick_queue0(&mut dev, &caps, &mut mem);

    assert_eq!(mem.get_slice(status, 1).unwrap()[0], VIRTIO_BLK_S_IOERR);
    assert!(backing.borrow().iter().all(|b| *b == 0));
    assert_eq!(read_u32_le(&mem, USED_RING + 8).unwrap(), 0);
}

#[test]
fn virtio_blk_rejects_requests_beyond_capacity() {
    let (mut dev, caps, mut mem, backing, _flushes) = setup();

    let header = 0x7000;
    let data = 0x8000;
    let status = 0x9000;

    // `setup()` uses a 4096-byte backing store -> 8 sectors. Sector 8 is out of range.
    let sector = 8u64;
    write_u32_le(&mut mem, header, VIRTIO_BLK_T_IN).unwrap();
    write_u32_le(&mut mem, header + 4, 0).unwrap();
    write_u64_le(&mut mem, header + 8, sector).unwrap();

    mem.write(data, &[0u8; 512]).unwrap();
    mem.write(status, &[0xff]).unwrap();

    write_desc(&mut mem, DESC_TABLE, 0, header, 16, 0x0001, 1);
    write_desc(&mut mem, DESC_TABLE, 1, data, 512, 0x0001 | 0x0002, 2);
    write_desc(&mut mem, DESC_TABLE, 2, status, 1, 0x0002, 0);

    write_u16_le(&mut mem, AVAIL_RING, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 1).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 4, 0).unwrap();
    write_u16_le(&mut mem, USED_RING, 0).unwrap();
    write_u16_le(&mut mem, USED_RING + 2, 0).unwrap();

    kick_queue0(&mut dev, &caps, &mut mem);

    assert_eq!(mem.get_slice(status, 1).unwrap()[0], VIRTIO_BLK_S_IOERR);
    assert!(backing.borrow().iter().all(|b| *b == 0));
    assert_eq!(read_u32_le(&mem, USED_RING + 8).unwrap(), 0);
}

#[test]
fn virtio_blk_accepts_aero_storage_virtual_disk_backend() {
    let (mut dev, caps, mut mem) = setup_aero_storage_disk();

    // WRITE request: OUT sector 2.
    let header = 0x7000;
    let data = 0x8000;
    let status = 0x9000;

    let sector = 2u64;
    write_u32_le(&mut mem, header, VIRTIO_BLK_T_OUT).unwrap();
    write_u32_le(&mut mem, header + 4, 0).unwrap();
    write_u64_le(&mut mem, header + 8, sector).unwrap();

    let payload: Vec<u8> = (0..512u16).flat_map(|v| v.to_le_bytes()).collect();
    mem.write(data, &payload).unwrap();
    mem.write(status, &[0xff]).unwrap();

    write_desc(&mut mem, DESC_TABLE, 0, header, 16, 0x0001, 1);
    write_desc(&mut mem, DESC_TABLE, 1, data, 1024, 0x0001, 2);
    write_desc(&mut mem, DESC_TABLE, 2, status, 1, 0x0002, 0);

    write_u16_le(&mut mem, AVAIL_RING, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 1).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 4, 0).unwrap();
    write_u16_le(&mut mem, USED_RING, 0).unwrap();
    write_u16_le(&mut mem, USED_RING + 2, 0).unwrap();

    kick_queue0(&mut dev, &caps, &mut mem);
    assert_eq!(mem.get_slice(status, 1).unwrap()[0], 0);

    // READ request: IN sector 2.
    let data2 = 0xA000;
    mem.write(data2, &vec![0u8; payload.len()]).unwrap();
    mem.write(status, &[0xff]).unwrap();

    write_u32_le(&mut mem, header, VIRTIO_BLK_T_IN).unwrap();
    write_u32_le(&mut mem, header + 4, 0).unwrap();
    write_u64_le(&mut mem, header + 8, sector).unwrap();

    write_desc(&mut mem, DESC_TABLE, 0, header, 16, 0x0001, 1);
    write_desc(&mut mem, DESC_TABLE, 1, data2, 1024, 0x0001 | 0x0002, 2);
    write_desc(&mut mem, DESC_TABLE, 2, status, 1, 0x0002, 0);

    // Add to avail ring at index 1.
    write_u16_le(&mut mem, AVAIL_RING + 4 + 2, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 2).unwrap();

    kick_queue0(&mut dev, &caps, &mut mem);

    let got = mem.get_slice(data2, payload.len()).unwrap();
    assert_eq!(got, payload.as_slice());
    assert_eq!(mem.get_slice(status, 1).unwrap()[0], 0);
}

#[test]
fn virtio_blk_virtual_disk_backend_maps_errors() {
    let disk = RawDisk::create(MemBackend::new(), 4096).unwrap();
    let mut backend: Box<dyn VirtualDisk + Send> = Box::new(disk);

    let mut buf = [0u8; 1];
    let err = BlockBackend::read_at(&mut backend, 4096, &mut buf).unwrap_err();
    assert_eq!(err, BlockBackendError::OutOfBounds);

    let err = BlockBackend::read_at(&mut backend, u64::MAX, &mut buf).unwrap_err();
    assert_eq!(err, BlockBackendError::IoError);

    let err = BlockBackend::write_at(&mut backend, 4096, &[0u8; 1]).unwrap_err();
    assert_eq!(err, BlockBackendError::OutOfBounds);

    let err = BlockBackend::write_at(&mut backend, u64::MAX, &[0u8; 1]).unwrap_err();
    assert_eq!(err, BlockBackendError::IoError);

    struct UnsupportedDisk;

    impl VirtualDisk for UnsupportedDisk {
        fn capacity_bytes(&self) -> u64 {
            512
        }

        fn read_at(&mut self, _offset: u64, _buf: &mut [u8]) -> aero_storage::Result<()> {
            Err(StorageDiskError::Unsupported("read"))
        }

        fn write_at(&mut self, _offset: u64, _buf: &[u8]) -> aero_storage::Result<()> {
            Err(StorageDiskError::Unsupported("write"))
        }

        fn flush(&mut self) -> aero_storage::Result<()> {
            Err(StorageDiskError::CorruptImage("flush"))
        }
    }

    let mut backend: Box<dyn VirtualDisk + Send> = Box::new(UnsupportedDisk);
    let err = BlockBackend::read_at(&mut backend, 0, &mut buf).unwrap_err();
    assert_eq!(err, BlockBackendError::IoError);
    let err = BlockBackend::write_at(&mut backend, 0, &[0u8; 1]).unwrap_err();
    assert_eq!(err, BlockBackendError::IoError);
    let err = BlockBackend::flush(&mut backend).unwrap_err();
    assert_eq!(err, BlockBackendError::IoError);
}

#[test]
fn virtio_blk_virtual_disk_backend_maps_browser_storage_failures_to_ioerr() {
    fn quota_exceeded() -> StorageDiskError {
        StorageDiskError::QuotaExceeded
    }

    fn in_use() -> StorageDiskError {
        StorageDiskError::InUse
    }

    fn invalid_state() -> StorageDiskError {
        StorageDiskError::InvalidState("closed".to_string())
    }

    fn backend_unavailable() -> StorageDiskError {
        StorageDiskError::BackendUnavailable
    }

    fn not_supported() -> StorageDiskError {
        StorageDiskError::NotSupported("opfs".to_string())
    }

    fn io_error() -> StorageDiskError {
        StorageDiskError::Io("boom".to_string())
    }

    struct ErrorDisk {
        read_err: fn() -> StorageDiskError,
        write_err: fn() -> StorageDiskError,
        flush_err: fn() -> StorageDiskError,
    }

    impl VirtualDisk for ErrorDisk {
        fn capacity_bytes(&self) -> u64 {
            512
        }

        fn read_at(&mut self, _offset: u64, _buf: &mut [u8]) -> aero_storage::Result<()> {
            Err((self.read_err)())
        }

        fn write_at(&mut self, _offset: u64, _buf: &[u8]) -> aero_storage::Result<()> {
            Err((self.write_err)())
        }

        fn flush(&mut self) -> aero_storage::Result<()> {
            Err((self.flush_err)())
        }
    }

    type DiskIoErrors = (
        fn() -> StorageDiskError,
        fn() -> StorageDiskError,
        fn() -> StorageDiskError,
    );

    let disks: &[DiskIoErrors] = &[
        (quota_exceeded, quota_exceeded, quota_exceeded),
        (in_use, in_use, in_use),
        (invalid_state, invalid_state, invalid_state),
        (backend_unavailable, backend_unavailable, backend_unavailable),
        (not_supported, not_supported, not_supported),
        (io_error, io_error, io_error),
    ];

    for (read_err, write_err, flush_err) in disks {
        let mut backend: Box<dyn VirtualDisk + Send> = Box::new(ErrorDisk {
            read_err: *read_err,
            write_err: *write_err,
            flush_err: *flush_err,
        });

        let mut buf = [0u8; 1];
        let err = BlockBackend::read_at(&mut backend, 0, &mut buf).unwrap_err();
        assert_eq!(err, BlockBackendError::IoError);

        let err = BlockBackend::write_at(&mut backend, 0, &[0u8; 1]).unwrap_err();
        assert_eq!(err, BlockBackendError::IoError);

        let err = BlockBackend::flush(&mut backend).unwrap_err();
        assert_eq!(err, BlockBackendError::IoError);
    }
}
