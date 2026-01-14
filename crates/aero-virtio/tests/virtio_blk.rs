use aero_io_snapshot::io::state::IoSnapshot;
use aero_platform::interrupts::msi::MsiMessage;
use aero_storage::{
    AeroSparseConfig, AeroSparseDisk, DiskError as StorageDiskError, MemBackend, RawDisk,
    VirtualDisk,
};
use aero_virtio::devices::blk::{
    VirtioBlk, VIRTIO_BLK_MAX_REQUEST_DATA_BYTES, VIRTIO_BLK_MAX_REQUEST_DESCRIPTORS,
    VIRTIO_BLK_SECTOR_SIZE, VIRTIO_BLK_S_IOERR, VIRTIO_BLK_S_UNSUPP, VIRTIO_BLK_T_DISCARD,
    VIRTIO_BLK_T_FLUSH, VIRTIO_BLK_T_GET_ID, VIRTIO_BLK_T_IN, VIRTIO_BLK_T_OUT,
    VIRTIO_BLK_T_WRITE_ZEROES, VIRTIO_BLK_WRITE_ZEROES_FLAG_UNMAP,
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
use aero_virtio::queue::{VIRTQ_DESC_F_INDIRECT, VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE};

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

const SECTOR_SIZE_BYTES: usize = VIRTIO_BLK_SECTOR_SIZE as usize;
const SECTOR_SIZE_U32: u32 = VIRTIO_BLK_SECTOR_SIZE as u32;

#[derive(Clone)]
struct SharedDisk {
    data: Arc<Mutex<Vec<u8>>>,
    flushes: Arc<AtomicU32>,
}

impl VirtualDisk for SharedDisk {
    fn capacity_bytes(&self) -> u64 {
        self.data.lock().unwrap().len() as u64
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> aero_storage::Result<()> {
        let data = self.data.lock().unwrap();
        let capacity = data.len() as u64;
        let end = offset
            .checked_add(buf.len() as u64)
            .ok_or(StorageDiskError::OffsetOverflow)?;
        if end > capacity {
            return Err(StorageDiskError::OutOfBounds {
                offset,
                len: buf.len(),
                capacity,
            });
        }
        let offset = offset as usize;
        let end = offset + buf.len();
        buf.copy_from_slice(&data[offset..end]);
        Ok(())
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> aero_storage::Result<()> {
        let mut data = self.data.lock().unwrap();
        let capacity = data.len() as u64;
        let end = offset
            .checked_add(buf.len() as u64)
            .ok_or(StorageDiskError::OffsetOverflow)?;
        if end > capacity {
            return Err(StorageDiskError::OutOfBounds {
                offset,
                len: buf.len(),
                capacity,
            });
        }
        let offset = offset as usize;
        let end = offset + buf.len();
        data[offset..end].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> aero_storage::Result<()> {
        self.flushes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[derive(Clone)]
struct TrackingDiscardDisk {
    data: Arc<Mutex<Vec<u8>>>,
    discards: Arc<AtomicU32>,
    writes: Arc<AtomicU32>,
}

impl VirtualDisk for TrackingDiscardDisk {
    fn capacity_bytes(&self) -> u64 {
        self.data.lock().unwrap().len() as u64
    }

    fn read_at(&mut self, offset: u64, dst: &mut [u8]) -> aero_storage::Result<()> {
        let data = self.data.lock().unwrap();
        let capacity = data.len() as u64;
        let end = offset
            .checked_add(dst.len() as u64)
            .ok_or(StorageDiskError::OffsetOverflow)?;
        if end > capacity {
            return Err(StorageDiskError::OutOfBounds {
                offset,
                len: dst.len(),
                capacity,
            });
        }
        let offset = offset as usize;
        let end = offset + dst.len();
        dst.copy_from_slice(&data[offset..end]);
        Ok(())
    }

    fn write_at(&mut self, offset: u64, src: &[u8]) -> aero_storage::Result<()> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        let mut data = self.data.lock().unwrap();
        let capacity = data.len() as u64;
        let end = offset
            .checked_add(src.len() as u64)
            .ok_or(StorageDiskError::OffsetOverflow)?;
        if end > capacity {
            return Err(StorageDiskError::OutOfBounds {
                offset,
                len: src.len(),
                capacity,
            });
        }
        let offset = offset as usize;
        let end = offset + src.len();
        data[offset..end].copy_from_slice(src);
        Ok(())
    }

    fn flush(&mut self) -> aero_storage::Result<()> {
        Ok(())
    }

    fn discard_range(&mut self, offset: u64, len: u64) -> aero_storage::Result<()> {
        self.discards.fetch_add(1, Ordering::SeqCst);
        let mut data = self.data.lock().unwrap();
        let capacity = data.len() as u64;
        if len == 0 {
            if offset > capacity {
                return Err(StorageDiskError::OutOfBounds {
                    offset,
                    len: 0,
                    capacity,
                });
            }
            return Ok(());
        }
        let end = offset
            .checked_add(len)
            .ok_or(StorageDiskError::OffsetOverflow)?;
        if end > capacity {
            return Err(StorageDiskError::OutOfBounds {
                offset,
                len: usize::try_from(len).unwrap_or(usize::MAX),
                capacity,
            });
        }
        let offset = offset as usize;
        let end = end as usize;
        data[offset..end].fill(0);
        Ok(())
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
        let cap_id = cfg[ptr];
        let next = cfg[ptr + 1] as usize;
        if cap_id == 0x09 {
            let cap_len = cfg[ptr + 2] as usize;
            let cfg_type = cfg[ptr + 3];
            let offset = u32::from_le_bytes(cfg[ptr + 8..ptr + 12].try_into().unwrap()) as u64;
            match cfg_type {
                VIRTIO_PCI_CAP_COMMON_CFG => caps.common = offset,
                VIRTIO_PCI_CAP_NOTIFY_CFG => {
                    caps.notify = offset;
                    caps.notify_mult =
                        u32::from_le_bytes(cfg[ptr + 16..ptr + 20].try_into().unwrap());
                }
                VIRTIO_PCI_CAP_ISR_CFG => caps.isr = offset,
                VIRTIO_PCI_CAP_DEVICE_CFG => caps.device = offset,
                _ => {}
            }
            assert!(cap_len >= 16);
        }
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
    Arc<Mutex<Vec<u8>>>,
    Arc<AtomicU32>,
);

#[derive(Clone, Default)]
struct TestIrq {
    legacy_count: Rc<Cell<u64>>,
    legacy_level: Rc<Cell<bool>>,
    msix_messages: Rc<RefCell<Vec<MsiMessage>>>,
}

impl TestIrq {
    fn legacy_count(&self) -> u64 {
        self.legacy_count.get()
    }

    fn legacy_level(&self) -> bool {
        self.legacy_level.get()
    }

    fn take_msix_messages(&self) -> Vec<MsiMessage> {
        core::mem::take(&mut *self.msix_messages.borrow_mut())
    }
}

impl InterruptSink for TestIrq {
    fn raise_legacy_irq(&mut self) {
        self.legacy_level.set(true);
        self.legacy_count
            .set(self.legacy_count.get().saturating_add(1));
    }

    fn lower_legacy_irq(&mut self) {
        self.legacy_level.set(false);
    }

    fn signal_msix(&mut self, message: MsiMessage) {
        self.msix_messages.borrow_mut().push(message);
    }
}

type SetupWithIrq = (
    VirtioPciDevice,
    Caps,
    GuestRam,
    Arc<Mutex<Vec<u8>>>,
    Arc<AtomicU32>,
    TestIrq,
);

type SetupTrackingDiscard = (
    VirtioPciDevice,
    Caps,
    GuestRam,
    Arc<Mutex<Vec<u8>>>,
    Arc<AtomicU32>,
    Arc<AtomicU32>,
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

    // Enable PCI memory decoding (BAR0 MMIO) + bus mastering (DMA). The virtio-pci transport gates
    // all guest-memory access on `PCI COMMAND.BME` (bit 2).
    dev.config_write(0x04, &0x0006u16.to_le_bytes());

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
    let backing = Arc::new(Mutex::new(vec![0u8; 4096]));
    let flushes = Arc::new(AtomicU32::new(0));
    let backend = SharedDisk {
        data: backing.clone(),
        flushes: flushes.clone(),
    };

    let blk = VirtioBlk::new(Box::new(backend));
    let dev = VirtioPciDevice::new(Box::new(blk), Box::new(InterruptLog::default()));

    let (dev, caps, mem) = setup_pci_device(dev);

    (dev, caps, mem, backing, flushes)
}

fn setup_with_sizes(disk_len: usize, mem_len: usize) -> Setup {
    let backing = Arc::new(Mutex::new(vec![0u8; disk_len]));
    let flushes = Arc::new(AtomicU32::new(0));
    let backend = SharedDisk {
        data: backing.clone(),
        flushes: flushes.clone(),
    };

    let blk = VirtioBlk::new(Box::new(backend));
    let dev = VirtioPciDevice::new(Box::new(blk), Box::new(InterruptLog::default()));

    let (dev, caps, _mem) = setup_pci_device(dev);
    let mem = GuestRam::new(mem_len);

    (dev, caps, mem, backing, flushes)
}

fn setup_tracking_discard_disk(disk_len: usize) -> SetupTrackingDiscard {
    let backing = Arc::new(Mutex::new(vec![0u8; disk_len]));
    let discards = Arc::new(AtomicU32::new(0));
    let writes = Arc::new(AtomicU32::new(0));
    let backend = TrackingDiscardDisk {
        data: backing.clone(),
        discards: discards.clone(),
        writes: writes.clone(),
    };

    let blk = VirtioBlk::new(Box::new(backend));
    let dev = VirtioPciDevice::new(Box::new(blk), Box::new(InterruptLog::default()));

    let (dev, caps, mem) = setup_pci_device(dev);

    (dev, caps, mem, backing, discards, writes)
}

fn setup_with_irq(irq: TestIrq) -> SetupWithIrq {
    let backing = Arc::new(Mutex::new(vec![0u8; 4096]));
    let flushes = Arc::new(AtomicU32::new(0));
    let backend = SharedDisk {
        data: backing.clone(),
        flushes: flushes.clone(),
    };

    let blk = VirtioBlk::new(Box::new(backend));
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
    let backend: Box<dyn VirtualDisk> = Box::new(disk);
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
    assert_eq!(blk_size, SECTOR_SIZE_U32);
}

#[test]
fn virtio_blk_config_advertises_write_zeroes_may_unmap() {
    let (mut dev, caps, _mem, _backing, _flushes) = setup();
    let mut buf = [0u8; 1];
    dev.bar0_read(caps.device + 56, &mut buf);
    assert_eq!(buf[0], 1);
}

#[test]
fn virtio_blk_config_read_out_of_range_offsets_return_zeroes() {
    let cfg = aero_virtio::devices::blk::VirtioBlkConfig {
        capacity: 8,
        size_max: 0,
        seg_max: 126,
        blk_size: SECTOR_SIZE_U32,
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
        blk_size: SECTOR_SIZE_U32,
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

    let payload: Vec<u8> = (0..(SECTOR_SIZE_BYTES as u16))
        .flat_map(|v| v.to_le_bytes())
        .collect();
    let (a, b) = payload.split_at(SECTOR_SIZE_BYTES);
    mem.write(data, a).unwrap();
    mem.write(data_b, b).unwrap();
    mem.write(status, &[0xff]).unwrap();

    write_desc(&mut mem, DESC_TABLE, 0, header, 16, 0x0001, 1);
    write_desc(&mut mem, DESC_TABLE, 1, data, SECTOR_SIZE_U32, 0x0001, 2);
    write_desc(&mut mem, DESC_TABLE, 2, data_b, SECTOR_SIZE_U32, 0x0001, 3);
    write_desc(&mut mem, DESC_TABLE, 3, status, 1, 0x0002, 0);

    write_u16_le(&mut mem, AVAIL_RING, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 1).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 4, 0).unwrap();

    write_u16_le(&mut mem, USED_RING, 0).unwrap();
    write_u16_le(&mut mem, USED_RING + 2, 0).unwrap();

    kick_queue0(&mut dev, &caps, &mut mem);

    {
        let backing = backing.lock().unwrap();
        assert_eq!(
            &backing[(sector * VIRTIO_BLK_SECTOR_SIZE) as usize
                ..(sector * VIRTIO_BLK_SECTOR_SIZE) as usize + payload.len()],
            payload.as_slice()
        );
    }
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
    write_desc(
        &mut mem,
        DESC_TABLE,
        1,
        data2,
        SECTOR_SIZE_U32,
        0x0001 | 0x0002,
        2,
    );
    write_desc(
        &mut mem,
        DESC_TABLE,
        2,
        data2_b,
        SECTOR_SIZE_U32,
        0x0001 | 0x0002,
        3,
    );
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
    // Use an undefined opcode (GET_ID is 8 and is supported).
    write_u32_le(&mut mem, header, 99).unwrap();
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
fn virtio_blk_get_id_returns_device_id() {
    let (mut dev, caps, mut mem, _backing, _flushes) = setup();

    let header = 0x7000;
    let id_buf = 0x8000;
    let status = 0x9000;

    write_u32_le(&mut mem, header, VIRTIO_BLK_T_GET_ID).unwrap();
    write_u32_le(&mut mem, header + 4, 0).unwrap();
    write_u64_le(&mut mem, header + 8, 0).unwrap();

    // Use a larger-than-required buffer and ensure the device writes only the 20-byte ID.
    let mut id_space = vec![0xa5u8; 32];
    mem.write(id_buf, &id_space).unwrap();
    mem.write(status, &[0xff]).unwrap();

    write_desc(&mut mem, DESC_TABLE, 0, header, 16, VIRTQ_DESC_F_NEXT, 1);
    write_desc(
        &mut mem,
        DESC_TABLE,
        1,
        id_buf,
        id_space.len() as u32,
        VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE,
        2,
    );
    write_desc(&mut mem, DESC_TABLE, 2, status, 1, VIRTQ_DESC_F_WRITE, 0);

    write_u16_le(&mut mem, AVAIL_RING, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 1).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 4, 0).unwrap();
    write_u16_le(&mut mem, USED_RING, 0).unwrap();
    write_u16_le(&mut mem, USED_RING + 2, 0).unwrap();

    kick_queue0(&mut dev, &caps, &mut mem);

    let got = mem.get_slice(id_buf, id_space.len()).unwrap();
    assert_eq!(&got[..20], b"aero-virtio-blk-id!!");
    // Bytes past the 20-byte ID are ignored.
    id_space[..20].copy_from_slice(b"aero-virtio-blk-id!!");
    assert_eq!(got, id_space.as_slice());
    assert_eq!(mem.get_slice(status, 1).unwrap()[0], 0);
    assert_eq!(read_u32_le(&mem, USED_RING + 8).unwrap(), 0);
}

#[test]
fn virtio_blk_discard_returns_ok() {
    let (mut dev, caps, mut mem, backing, _flushes) = setup();

    // Make part of the discarded range non-zero, but leave the first sector zero. This ensures the
    // device cannot rely on checking only the first sector when deciding whether to fall back to
    // explicit zero writes.
    {
        let mut backing = backing.lock().unwrap();
        backing[1024..1536].fill(0xa5);
    }

    let header = 0x7000;
    let seg = 0x8000;
    let status = 0x9000;

    write_u32_le(&mut mem, header, VIRTIO_BLK_T_DISCARD).unwrap();
    write_u32_le(&mut mem, header + 4, 0).unwrap();
    write_u64_le(&mut mem, header + 8, 0).unwrap();

    // One discard segment: sector 1, length 2 sectors, flags 0.
    write_u64_le(&mut mem, seg, 1).unwrap();
    write_u32_le(&mut mem, seg + 8, 2).unwrap();
    write_u32_le(&mut mem, seg + 12, 0).unwrap();

    mem.write(status, &[0xff]).unwrap();

    write_desc(&mut mem, DESC_TABLE, 0, header, 16, 0x0001, 1);
    write_desc(&mut mem, DESC_TABLE, 1, seg, 16, 0x0001, 2);
    write_desc(&mut mem, DESC_TABLE, 2, status, 1, 0x0002, 0);

    write_u16_le(&mut mem, AVAIL_RING, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 1).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 4, 0).unwrap();
    write_u16_le(&mut mem, USED_RING, 0).unwrap();
    write_u16_le(&mut mem, USED_RING + 2, 0).unwrap();

    kick_queue0(&mut dev, &caps, &mut mem);

    assert_eq!(mem.get_slice(status, 1).unwrap()[0], 0);
    {
        let backing = backing.lock().unwrap();
        assert!(backing[SECTOR_SIZE_BYTES..SECTOR_SIZE_BYTES * 3]
            .iter()
            .all(|b| *b == 0));
    }
}

#[test]
fn virtio_blk_discard_multi_segment_zeroes_ranges_best_effort() {
    let (mut dev, caps, mut mem, backing, _flushes) = setup();

    // Fill the whole disk with non-zero bytes, then DISCARD two disjoint ranges.
    backing.lock().unwrap().fill(0xa5);

    let header = 0x7000;
    let segs = 0x8000;
    let status = 0x9000;

    write_u32_le(&mut mem, header, VIRTIO_BLK_T_DISCARD).unwrap();
    write_u32_le(&mut mem, header + 4, 0).unwrap();
    write_u64_le(&mut mem, header + 8, 0).unwrap();

    // Two segments:
    // - sector 1, 1 sector
    // - sector 3, 2 sectors
    write_u64_le(&mut mem, segs, 1).unwrap();
    write_u32_le(&mut mem, segs + 8, 1).unwrap();
    write_u32_le(&mut mem, segs + 12, 0).unwrap();
    write_u64_le(&mut mem, segs + 16, 3).unwrap();
    write_u32_le(&mut mem, segs + 24, 2).unwrap();
    write_u32_le(&mut mem, segs + 28, 0).unwrap();

    mem.write(status, &[0xff]).unwrap();

    write_desc(&mut mem, DESC_TABLE, 0, header, 16, 0x0001, 1);
    write_desc(&mut mem, DESC_TABLE, 1, segs, 32, 0x0001, 2);
    write_desc(&mut mem, DESC_TABLE, 2, status, 1, 0x0002, 0);

    write_u16_le(&mut mem, AVAIL_RING, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 1).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 4, 0).unwrap();
    write_u16_le(&mut mem, USED_RING, 0).unwrap();
    write_u16_le(&mut mem, USED_RING + 2, 0).unwrap();

    kick_queue0(&mut dev, &caps, &mut mem);

    assert_eq!(mem.get_slice(status, 1).unwrap()[0], 0);

    // Confirm only the discarded ranges were zeroed.
    {
        let backing = backing.lock().unwrap();
        assert!(backing[0..SECTOR_SIZE_BYTES].iter().all(|b| *b == 0xa5));
        assert!(backing[SECTOR_SIZE_BYTES..SECTOR_SIZE_BYTES * 2]
            .iter()
            .all(|b| *b == 0));
        assert!(backing[1024..1536].iter().all(|b| *b == 0xa5));
        assert!(backing[1536..2560].iter().all(|b| *b == 0));
        assert!(backing[2560..4096].iter().all(|b| *b == 0xa5));
    }
}

#[test]
fn virtio_blk_discard_rejects_out_of_bounds_requests() {
    let (mut dev, caps, mut mem, backing, _flushes) = setup();

    // Mark the whole disk so we can validate the failed request doesn't modify it.
    backing.lock().unwrap().fill(0xa5);

    let header = 0x7000;
    let seg = 0x8000;
    let status = 0x9000;

    write_u32_le(&mut mem, header, VIRTIO_BLK_T_DISCARD).unwrap();
    write_u32_le(&mut mem, header + 4, 0).unwrap();
    write_u64_le(&mut mem, header + 8, 0).unwrap();

    // `setup()` uses a 4096-byte backing store -> 8 sectors. Sector 7 + 2 sectors overflows.
    write_u64_le(&mut mem, seg, 7).unwrap();
    write_u32_le(&mut mem, seg + 8, 2).unwrap();
    write_u32_le(&mut mem, seg + 12, 0).unwrap();

    mem.write(status, &[0xff]).unwrap();

    write_desc(&mut mem, DESC_TABLE, 0, header, 16, 0x0001, 1);
    write_desc(&mut mem, DESC_TABLE, 1, seg, 16, 0x0001, 2);
    write_desc(&mut mem, DESC_TABLE, 2, status, 1, 0x0002, 0);

    write_u16_le(&mut mem, AVAIL_RING, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 1).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 4, 0).unwrap();
    write_u16_le(&mut mem, USED_RING, 0).unwrap();
    write_u16_le(&mut mem, USED_RING + 2, 0).unwrap();

    kick_queue0(&mut dev, &caps, &mut mem);

    assert_eq!(mem.get_slice(status, 1).unwrap()[0], VIRTIO_BLK_S_IOERR);
    assert!(backing.lock().unwrap().iter().all(|b| *b == 0xa5));
}

#[test]
fn virtio_blk_discard_reclaims_sparse_blocks_and_reads_zero() {
    // Use a real AeroSparseDisk so DISCARD can reclaim storage by clearing allocation table entries
    // (reads of discarded blocks return zeros).
    let sectors = 4096u64; // 2 MiB
    let capacity_bytes = sectors * VIRTIO_BLK_SECTOR_SIZE;
    let disk = AeroSparseDisk::create(
        MemBackend::new(),
        AeroSparseConfig {
            disk_size_bytes: capacity_bytes,
            block_size_bytes: 1024 * 1024,
        },
    )
    .unwrap();
    let backend: Box<dyn VirtualDisk> = Box::new(disk);
    let blk = VirtioBlk::new(backend);
    let dev = VirtioPciDevice::new(Box::new(blk), Box::new(InterruptLog::default()));
    let (mut dev, caps, mut mem) = setup_pci_device(dev);

    let header = 0x7000;
    let data = 0x8000;
    let seg = 0xA000;
    let read_buf = 0xB000;
    let status = 0x9000;

    // OUT: write non-zero data at sector 0.
    write_u32_le(&mut mem, header, VIRTIO_BLK_T_OUT).unwrap();
    write_u32_le(&mut mem, header + 4, 0).unwrap();
    write_u64_le(&mut mem, header + 8, 0).unwrap();

    let payload = vec![0x5A; SECTOR_SIZE_BYTES];
    mem.write(data, &payload).unwrap();
    mem.write(status, &[0xff]).unwrap();

    write_desc(&mut mem, DESC_TABLE, 0, header, 16, 0x0001, 1);
    write_desc(&mut mem, DESC_TABLE, 1, data, SECTOR_SIZE_U32, 0x0001, 2);
    write_desc(&mut mem, DESC_TABLE, 2, status, 1, 0x0002, 0);

    write_u16_le(&mut mem, AVAIL_RING, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 1).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 4, 0).unwrap();
    write_u16_le(&mut mem, USED_RING, 0).unwrap();
    write_u16_le(&mut mem, USED_RING + 2, 0).unwrap();

    kick_queue0(&mut dev, &caps, &mut mem);
    assert_eq!(mem.get_slice(status, 1).unwrap()[0], 0);

    // DISCARD: discard the entire first sparse allocation block (1 MiB / 512B = 2048 sectors).
    write_u32_le(&mut mem, header, VIRTIO_BLK_T_DISCARD).unwrap();
    write_u32_le(&mut mem, header + 4, 0).unwrap();
    write_u64_le(&mut mem, header + 8, 0).unwrap();

    write_u64_le(&mut mem, seg, 0).unwrap(); // sector
    write_u32_le(&mut mem, seg + 8, 2048).unwrap(); // num_sectors
    write_u32_le(&mut mem, seg + 12, 0).unwrap(); // flags

    mem.write(status, &[0xff]).unwrap();
    write_desc(&mut mem, DESC_TABLE, 0, header, 16, 0x0001, 1);
    write_desc(&mut mem, DESC_TABLE, 1, seg, 16, 0x0001, 2);
    write_desc(&mut mem, DESC_TABLE, 2, status, 1, 0x0002, 0);

    // Add to avail ring at index 1.
    write_u16_le(&mut mem, AVAIL_RING + 4 + 2, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 2).unwrap();

    kick_queue0(&mut dev, &caps, &mut mem);
    assert_eq!(mem.get_slice(status, 1).unwrap()[0], 0);

    // IN: read sector 0 and ensure it is now zero-filled.
    mem.write(read_buf, &[0xccu8; SECTOR_SIZE_BYTES]).unwrap();
    mem.write(status, &[0xff]).unwrap();
    write_u32_le(&mut mem, header, VIRTIO_BLK_T_IN).unwrap();
    write_u32_le(&mut mem, header + 4, 0).unwrap();
    write_u64_le(&mut mem, header + 8, 0).unwrap();

    write_desc(&mut mem, DESC_TABLE, 0, header, 16, 0x0001, 1);
    write_desc(
        &mut mem,
        DESC_TABLE,
        1,
        read_buf,
        SECTOR_SIZE_U32,
        0x0001 | 0x0002,
        2,
    );
    write_desc(&mut mem, DESC_TABLE, 2, status, 1, 0x0002, 0);

    // Add to avail ring at index 2.
    write_u16_le(&mut mem, AVAIL_RING + 4 + 4, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 3).unwrap();

    kick_queue0(&mut dev, &caps, &mut mem);
    assert_eq!(mem.get_slice(status, 1).unwrap()[0], 0);

    let out = mem.get_slice(read_buf, SECTOR_SIZE_BYTES).unwrap();
    assert!(out.iter().all(|b| *b == 0));
}

#[test]
fn virtio_blk_write_zeroes_writes_zeroes_and_returns_ok() {
    let (mut dev, caps, mut mem, backing, _flushes) = setup();

    let header = 0x7000;
    let seg = 0x8000;
    let status = 0x9000;

    // Fill sectors 2..4 with non-zero data, then request WRITE_ZEROES for the same range.
    let sector = 2u64;
    let num_sectors = 2u32;
    let off = (sector * VIRTIO_BLK_SECTOR_SIZE) as usize;
    let len = (u64::from(num_sectors) * VIRTIO_BLK_SECTOR_SIZE) as usize;
    {
        let mut backing = backing.lock().unwrap();
        backing[off..off + len].fill(0xa5);
    }

    write_u32_le(&mut mem, header, VIRTIO_BLK_T_WRITE_ZEROES).unwrap();
    write_u32_le(&mut mem, header + 4, 0).unwrap();
    write_u64_le(&mut mem, header + 8, 0).unwrap();

    // One write-zeroes segment.
    write_u64_le(&mut mem, seg, sector).unwrap();
    write_u32_le(&mut mem, seg + 8, num_sectors).unwrap();
    write_u32_le(&mut mem, seg + 12, 0).unwrap();

    mem.write(status, &[0xff]).unwrap();

    write_desc(&mut mem, DESC_TABLE, 0, header, 16, 0x0001, 1);
    write_desc(&mut mem, DESC_TABLE, 1, seg, 16, 0x0001, 2);
    write_desc(&mut mem, DESC_TABLE, 2, status, 1, 0x0002, 0);

    write_u16_le(&mut mem, AVAIL_RING, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 1).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 4, 0).unwrap();
    write_u16_le(&mut mem, USED_RING, 0).unwrap();
    write_u16_le(&mut mem, USED_RING + 2, 0).unwrap();

    kick_queue0(&mut dev, &caps, &mut mem);

    assert_eq!(mem.get_slice(status, 1).unwrap()[0], 0);
    {
        let backing = backing.lock().unwrap();
        assert!(backing[off..off + len].iter().all(|b| *b == 0));
    }
}

#[test]
fn virtio_blk_write_zeroes_unmap_prefers_discard_range_when_possible() {
    let (mut dev, caps, mut mem, backing, discards, writes) = setup_tracking_discard_disk(4096);

    // Pre-fill sector 1 with non-zero bytes, then request WRITE_ZEROES with the UNMAP flag.
    backing.lock().unwrap()[SECTOR_SIZE_BYTES..SECTOR_SIZE_BYTES * 2].fill(0xa5);

    let header = 0x7000;
    let seg = 0x8000;
    let status = 0x9000;

    write_u32_le(&mut mem, header, VIRTIO_BLK_T_WRITE_ZEROES).unwrap();
    write_u32_le(&mut mem, header + 4, 0).unwrap();
    write_u64_le(&mut mem, header + 8, 0).unwrap();

    write_u64_le(&mut mem, seg, 1).unwrap();
    write_u32_le(&mut mem, seg + 8, 1).unwrap();
    write_u32_le(&mut mem, seg + 12, VIRTIO_BLK_WRITE_ZEROES_FLAG_UNMAP).unwrap();

    mem.write(status, &[0xff]).unwrap();

    write_desc(&mut mem, DESC_TABLE, 0, header, 16, 0x0001, 1);
    write_desc(&mut mem, DESC_TABLE, 1, seg, 16, 0x0001, 2);
    write_desc(&mut mem, DESC_TABLE, 2, status, 1, 0x0002, 0);

    write_u16_le(&mut mem, AVAIL_RING, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 1).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 4, 0).unwrap();
    write_u16_le(&mut mem, USED_RING, 0).unwrap();
    write_u16_le(&mut mem, USED_RING + 2, 0).unwrap();

    kick_queue0(&mut dev, &caps, &mut mem);

    assert_eq!(mem.get_slice(status, 1).unwrap()[0], 0);
    assert_eq!(
        discards.load(Ordering::SeqCst),
        1,
        "UNMAP must call discard_range()"
    );
    assert_eq!(
        writes.load(Ordering::SeqCst),
        0,
        "discard_range() returned zeros; device should not re-write zeros"
    );
    assert!(
        backing.lock().unwrap()[SECTOR_SIZE_BYTES..SECTOR_SIZE_BYTES * 2]
            .iter()
            .all(|b| *b == 0)
    );
}

#[test]
fn virtio_blk_write_zeroes_multi_segment_spans_multiple_data_descriptors() {
    let (mut dev, caps, mut mem, backing, _flushes) = setup();

    // Fill the whole disk with non-zero bytes, then WRITE_ZEROES two disjoint sectors using a
    // segment table that crosses descriptor boundaries mid-segment.
    backing.lock().unwrap().fill(0xa5);

    let header = 0x7000;
    let seg_a = 0x8000;
    let seg_b = 0x9000;
    let status = 0xA000;

    write_u32_le(&mut mem, header, VIRTIO_BLK_T_WRITE_ZEROES).unwrap();
    write_u32_le(&mut mem, header + 4, 0).unwrap();
    write_u64_le(&mut mem, header + 8, 0).unwrap();

    // Two write-zeroes segments:
    // - sector 1, 1 sector
    // - sector 4, 1 sector
    let mut table = [0u8; 32];
    table[0..8].copy_from_slice(&1u64.to_le_bytes());
    table[8..12].copy_from_slice(&1u32.to_le_bytes());
    table[12..16].copy_from_slice(&0u32.to_le_bytes());
    table[16..24].copy_from_slice(&4u64.to_le_bytes());
    table[24..28].copy_from_slice(&1u32.to_le_bytes());
    table[28..32].copy_from_slice(&0u32.to_le_bytes());

    // Split the segment table at 20 bytes: 16 bytes for segment 0 plus 4 bytes of segment 1.
    mem.write(seg_a, &table[..20]).unwrap();
    mem.write(seg_b, &table[20..]).unwrap();

    mem.write(status, &[0xff]).unwrap();

    write_desc(&mut mem, DESC_TABLE, 0, header, 16, 0x0001, 1);
    write_desc(&mut mem, DESC_TABLE, 1, seg_a, 20, 0x0001, 2);
    write_desc(&mut mem, DESC_TABLE, 2, seg_b, 12, 0x0001, 3);
    write_desc(&mut mem, DESC_TABLE, 3, status, 1, 0x0002, 0);

    write_u16_le(&mut mem, AVAIL_RING, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 1).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 4, 0).unwrap();
    write_u16_le(&mut mem, USED_RING, 0).unwrap();
    write_u16_le(&mut mem, USED_RING + 2, 0).unwrap();

    kick_queue0(&mut dev, &caps, &mut mem);

    assert_eq!(mem.get_slice(status, 1).unwrap()[0], 0);

    // Confirm only the requested sectors were zeroed.
    {
        let backing = backing.lock().unwrap();
        assert!(backing[0..SECTOR_SIZE_BYTES].iter().all(|b| *b == 0xa5));
        assert!(backing[SECTOR_SIZE_BYTES..SECTOR_SIZE_BYTES * 2]
            .iter()
            .all(|b| *b == 0));
        assert!(backing[1024..2048].iter().all(|b| *b == 0xa5));
        assert!(backing[2048..2560].iter().all(|b| *b == 0));
        assert!(backing[2560..4096].iter().all(|b| *b == 0xa5));
    }
}

#[test]
fn virtio_blk_write_zeroes_rejects_out_of_bounds_requests() {
    let (mut dev, caps, mut mem, backing, _flushes) = setup();

    let header = 0x7000;
    let seg = 0x8000;
    let status = 0x9000;

    // Mark the whole disk so we can validate the failed request doesn't modify it.
    backing.lock().unwrap().fill(0xa5);

    write_u32_le(&mut mem, header, VIRTIO_BLK_T_WRITE_ZEROES).unwrap();
    write_u32_le(&mut mem, header + 4, 0).unwrap();
    write_u64_le(&mut mem, header + 8, 0).unwrap();

    // `setup()` uses a 4096-byte backing store -> 8 sectors. Sector 7 + 2 sectors overflows.
    write_u64_le(&mut mem, seg, 7).unwrap();
    write_u32_le(&mut mem, seg + 8, 2).unwrap();
    write_u32_le(&mut mem, seg + 12, 0).unwrap();

    mem.write(status, &[0xff]).unwrap();

    write_desc(&mut mem, DESC_TABLE, 0, header, 16, 0x0001, 1);
    write_desc(&mut mem, DESC_TABLE, 1, seg, 16, 0x0001, 2);
    write_desc(&mut mem, DESC_TABLE, 2, status, 1, 0x0002, 0);

    write_u16_le(&mut mem, AVAIL_RING, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 1).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 4, 0).unwrap();
    write_u16_le(&mut mem, USED_RING, 0).unwrap();
    write_u16_le(&mut mem, USED_RING + 2, 0).unwrap();

    kick_queue0(&mut dev, &caps, &mut mem);

    assert_eq!(mem.get_slice(status, 1).unwrap()[0], VIRTIO_BLK_S_IOERR);
    assert!(backing.lock().unwrap().iter().all(|b| *b == 0xa5));
}

#[test]
fn virtio_blk_write_zeroes_rejects_oversize_requests() {
    let max_sectors = VIRTIO_BLK_MAX_REQUEST_DATA_BYTES / VIRTIO_BLK_SECTOR_SIZE;
    let num_sectors = u32::try_from(max_sectors + 1).unwrap();
    let disk_len = usize::try_from(u64::from(num_sectors) * VIRTIO_BLK_SECTOR_SIZE).unwrap();

    let (mut dev, caps, mut mem, backing, _flushes) = setup_with_sizes(disk_len, 0x10000);

    let header = 0x7000;
    let seg = 0x8000;
    let status = 0x9000;

    // Mark the whole disk so we can validate the failed request doesn't modify it.
    backing.lock().unwrap().fill(0xa5);

    write_u32_le(&mut mem, header, VIRTIO_BLK_T_WRITE_ZEROES).unwrap();
    write_u32_le(&mut mem, header + 4, 0).unwrap();
    write_u64_le(&mut mem, header + 8, 0).unwrap();

    // Request WRITE_ZEROES over the entire disk, but exceed the device's per-request cap.
    write_u64_le(&mut mem, seg, 0).unwrap();
    write_u32_le(&mut mem, seg + 8, num_sectors).unwrap();
    write_u32_le(&mut mem, seg + 12, 0).unwrap();

    mem.write(status, &[0xff]).unwrap();

    write_desc(&mut mem, DESC_TABLE, 0, header, 16, 0x0001, 1);
    write_desc(&mut mem, DESC_TABLE, 1, seg, 16, 0x0001, 2);
    write_desc(&mut mem, DESC_TABLE, 2, status, 1, 0x0002, 0);

    write_u16_le(&mut mem, AVAIL_RING, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 1).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 4, 0).unwrap();
    write_u16_le(&mut mem, USED_RING, 0).unwrap();
    write_u16_le(&mut mem, USED_RING + 2, 0).unwrap();

    kick_queue0(&mut dev, &caps, &mut mem);

    assert_eq!(mem.get_slice(status, 1).unwrap()[0], VIRTIO_BLK_S_IOERR);
    assert_eq!(dev.debug_queue_used_idx(&mem, 0), Some(1));
    assert!(backing.lock().unwrap().iter().all(|b| *b == 0xa5));
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
    assert_eq!(flushes.load(Ordering::SeqCst), 1);
    assert_eq!(read_u32_le(&mem, USED_RING + 8).unwrap(), 0);
}

#[test]
fn virtio_blk_msix_queue_interrupts_use_programmed_msix_message_and_do_not_fallback_to_intx() {
    let irq0 = TestIrq::default();
    let (mut dev, caps, mut mem, _backing, flushes, irq0) = setup_with_irq(irq0);

    // Find MSI-X capability in PCI config space.
    let mut cfg = [0u8; 256];
    dev.config_read(0, &mut cfg);
    let mut ptr = cfg[0x34] as usize;
    let mut msix_cap_offset = None;
    while ptr != 0 {
        if cfg[ptr] == 0x11 {
            msix_cap_offset = Some(ptr as u16);
            break;
        }
        ptr = cfg[ptr + 1] as usize;
    }
    let msix_cap_offset = msix_cap_offset.expect("missing MSI-X capability");

    // Enable MSI-X.
    let msg_ctl = u16::from_le_bytes([
        cfg[msix_cap_offset as usize + 0x02],
        cfg[msix_cap_offset as usize + 0x03],
    ]);
    dev.config_write(msix_cap_offset + 0x02, &(msg_ctl | (1 << 15)).to_le_bytes());

    // MSI-X Table register: BIR + offset.
    let table = u32::from_le_bytes(
        cfg[msix_cap_offset as usize + 0x04..msix_cap_offset as usize + 0x08]
            .try_into()
            .unwrap(),
    );
    assert_eq!(table & 0x7, 0, "MSI-X table must live in BAR0");
    let table_offset = (table & !0x7) as u64;

    // Program table entry 1 (queue 0 vector: index 1 because index 0 is the config vector).
    let entry = table_offset + 16;
    bar_write_u32(&mut dev, entry, 0xfee0_0000);
    bar_write_u32(&mut dev, entry + 0x04, 0);
    bar_write_u32(&mut dev, entry + 0x08, 0x0045);
    bar_write_u32(&mut dev, entry + 0x0c, 0); // unmasked

    // Build a FLUSH request so completion is observable via `flushes`.
    let header = 0x7000;
    let status = 0x9000;

    write_u32_le(&mut mem, header, VIRTIO_BLK_T_FLUSH).unwrap();
    write_u32_le(&mut mem, header + 4, 0).unwrap();
    write_u64_le(&mut mem, header + 8, 0).unwrap();
    mem.write(status, &[0xff]).unwrap();

    write_desc(&mut mem, DESC_TABLE, 0, header, 16, 0x0001, 1);
    write_desc(&mut mem, DESC_TABLE, 1, status, 1, 0x0002, 0);

    // Initialise rings (flags/idx).
    write_u16_le(&mut mem, AVAIL_RING, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 1).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 4, 0).unwrap();
    write_u16_le(&mut mem, USED_RING, 0).unwrap();
    write_u16_le(&mut mem, USED_RING + 2, 0).unwrap();

    // With MSI-X enabled but no queue vector assigned, virtio-pci must suppress interrupts and
    // not fall back to legacy INTx.
    kick_queue0(&mut dev, &caps, &mut mem);
    assert_eq!(mem.get_slice(status, 1).unwrap()[0], 0);
    assert_eq!(flushes.load(Ordering::SeqCst), 1);
    assert_eq!(irq0.legacy_count(), 0);
    assert!(!irq0.legacy_level());
    assert!(irq0.take_msix_messages().is_empty());

    // Assign the queue MSI-X vector and submit another request; now the MSI message should fire.
    bar_write_u16(&mut dev, caps.common + 0x16, 0); // queue_select
    bar_write_u16(&mut dev, caps.common + 0x1a, 1); // queue_msix_vector

    mem.write(status, &[0xff]).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 4 + 2, 0).unwrap(); // ring[1] = head 0
    write_u16_le(&mut mem, AVAIL_RING + 2, 2).unwrap(); // idx = 2

    kick_queue0(&mut dev, &caps, &mut mem);
    assert_eq!(mem.get_slice(status, 1).unwrap()[0], 0);
    assert_eq!(flushes.load(Ordering::SeqCst), 2);
    assert_eq!(irq0.legacy_count(), 0);
    assert!(!irq0.legacy_level());
    assert_eq!(
        irq0.take_msix_messages(),
        vec![MsiMessage {
            address: 0xfee0_0000,
            data: 0x0045,
        }]
    );
}

#[test]
fn virtio_blk_msix_pending_bit_redelivered_on_unmask() {
    let irq0 = TestIrq::default();
    let (mut dev, caps, mut mem, _backing, flushes, irq0) = setup_with_irq(irq0);

    // Find MSI-X capability in PCI config space.
    let mut cfg = [0u8; 256];
    dev.config_read(0, &mut cfg);
    let mut ptr = cfg[0x34] as usize;
    let mut msix_cap_offset = None;
    while ptr != 0 {
        if cfg[ptr] == 0x11 {
            msix_cap_offset = Some(ptr as u16);
            break;
        }
        ptr = cfg[ptr + 1] as usize;
    }
    let msix_cap_offset = msix_cap_offset.expect("missing MSI-X capability");

    // Enable MSI-X.
    let msg_ctl = u16::from_le_bytes([
        cfg[msix_cap_offset as usize + 0x02],
        cfg[msix_cap_offset as usize + 0x03],
    ]);
    dev.config_write(msix_cap_offset + 0x02, &(msg_ctl | (1 << 15)).to_le_bytes());

    // MSI-X Table register: BIR + offset.
    let table = u32::from_le_bytes(
        cfg[msix_cap_offset as usize + 0x04..msix_cap_offset as usize + 0x08]
            .try_into()
            .unwrap(),
    );
    assert_eq!(table & 0x7, 0, "MSI-X table must live in BAR0");
    let table_offset = (table & !0x7) as u64;

    let pba = u32::from_le_bytes(
        cfg[msix_cap_offset as usize + 0x08..msix_cap_offset as usize + 0x0c]
            .try_into()
            .unwrap(),
    );
    assert_eq!(pba & 0x7, 0, "MSI-X PBA must live in BAR0");
    let pba_offset = (pba & !0x7) as u64;

    // Program table entry 1 (queue 0 vector: index 1 because index 0 is the config vector), but
    // keep it masked so the first completion sets the PBA pending bit instead of delivering the
    // message.
    let entry = table_offset + 16;
    bar_write_u32(&mut dev, entry, 0xfee0_0000);
    bar_write_u32(&mut dev, entry + 0x04, 0);
    bar_write_u32(&mut dev, entry + 0x08, 0x0045);
    bar_write_u32(&mut dev, entry + 0x0c, 1); // masked

    // Assign the queue MSI-X vector.
    bar_write_u16(&mut dev, caps.common + 0x16, 0); // queue_select
    bar_write_u16(&mut dev, caps.common + 0x1a, 1); // queue_msix_vector

    // Build a FLUSH request so completion is observable via `flushes`.
    let header = 0x7000;
    let status = 0x9000;

    write_u32_le(&mut mem, header, VIRTIO_BLK_T_FLUSH).unwrap();
    write_u32_le(&mut mem, header + 4, 0).unwrap();
    write_u64_le(&mut mem, header + 8, 0).unwrap();
    mem.write(status, &[0xff]).unwrap();

    write_desc(&mut mem, DESC_TABLE, 0, header, 16, 0x0001, 1);
    write_desc(&mut mem, DESC_TABLE, 1, status, 1, 0x0002, 0);

    // Initialise rings (flags/idx).
    write_u16_le(&mut mem, AVAIL_RING, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 1).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 4, 0).unwrap();
    write_u16_le(&mut mem, USED_RING, 0).unwrap();
    write_u16_le(&mut mem, USED_RING + 2, 0).unwrap();

    kick_queue0(&mut dev, &caps, &mut mem);

    assert_eq!(mem.get_slice(status, 1).unwrap()[0], 0);
    assert_eq!(flushes.load(Ordering::SeqCst), 1);

    // MSI-X is enabled and a vector is assigned, but it is masked. This must set the PBA pending
    // bit without falling back to INTx.
    assert_eq!(irq0.legacy_count(), 0);
    assert!(!irq0.legacy_level());
    assert!(irq0.take_msix_messages().is_empty());

    let pending = bar_read_u64(&mut dev, pba_offset);
    assert_eq!(
        pending & (1 << 1),
        1 << 1,
        "vector 1 should be pending in PBA"
    );

    // Clear the virtio interrupt cause (ISR is read-to-clear). Pending MSI-X delivery should still
    // occur once the entry becomes unmasked, even without a new interrupt edge.
    let mut _isr = [0u8; 1];
    dev.bar0_read(caps.isr, &mut _isr);
    let pending = bar_read_u64(&mut dev, pba_offset);
    assert_eq!(
        pending & (1 << 1),
        1 << 1,
        "PBA pending bit should remain set after clearing the ISR"
    );

    // Unmask the entry; the device should immediately re-deliver the pending message.
    bar_write_u32(&mut dev, entry + 0x0c, 0); // unmasked
    assert_eq!(
        flushes.load(Ordering::SeqCst),
        1,
        "unmask must not require a new completion"
    );
    assert_eq!(
        irq0.take_msix_messages(),
        vec![MsiMessage {
            address: 0xfee0_0000,
            data: 0x0045,
        }]
    );

    let pending = bar_read_u64(&mut dev, pba_offset);
    assert_eq!(
        pending & (1 << 1),
        0,
        "vector 1 pending bit must be cleared"
    );
}

#[test]
fn virtio_blk_msix_function_mask_pending_bit_redelivered_on_unmask_even_after_isr_cleared() {
    let irq0 = TestIrq::default();
    let (mut dev, caps, mut mem, _backing, flushes, irq0) = setup_with_irq(irq0);

    // Find MSI-X capability in PCI config space.
    let mut cfg = [0u8; 256];
    dev.config_read(0, &mut cfg);
    let mut ptr = cfg[0x34] as usize;
    let mut msix_cap_offset = None;
    while ptr != 0 {
        if cfg[ptr] == 0x11 {
            msix_cap_offset = Some(ptr as u16);
            break;
        }
        ptr = cfg[ptr + 1] as usize;
    }
    let msix_cap_offset = msix_cap_offset.expect("missing MSI-X capability");

    // Enable MSI-X and set Function Mask (bit 14).
    let msg_ctl = u16::from_le_bytes([
        cfg[msix_cap_offset as usize + 0x02],
        cfg[msix_cap_offset as usize + 0x03],
    ]);
    dev.config_write(
        msix_cap_offset + 0x02,
        &(msg_ctl | (1 << 15) | (1 << 14)).to_le_bytes(),
    );

    // MSI-X Table + PBA register: BIR + offset.
    let table = u32::from_le_bytes(
        cfg[msix_cap_offset as usize + 0x04..msix_cap_offset as usize + 0x08]
            .try_into()
            .unwrap(),
    );
    assert_eq!(table & 0x7, 0, "MSI-X table must live in BAR0");
    let table_offset = (table & !0x7) as u64;

    let pba = u32::from_le_bytes(
        cfg[msix_cap_offset as usize + 0x08..msix_cap_offset as usize + 0x0c]
            .try_into()
            .unwrap(),
    );
    assert_eq!(pba & 0x7, 0, "MSI-X PBA must live in BAR0");
    let pba_offset = (pba & !0x7) as u64;

    // Program table entry 1 (queue 0 vector: index 1 because index 0 is the config vector).
    let entry = table_offset + 16;
    bar_write_u32(&mut dev, entry, 0xfee0_0000);
    bar_write_u32(&mut dev, entry + 0x04, 0);
    bar_write_u32(&mut dev, entry + 0x08, 0x0045);
    bar_write_u32(&mut dev, entry + 0x0c, 0); // unmasked

    // Assign the queue MSI-X vector.
    bar_write_u16(&mut dev, caps.common + 0x16, 0); // queue_select
    bar_write_u16(&mut dev, caps.common + 0x1a, 1); // queue_msix_vector

    // Build a FLUSH request so completion is observable via `flushes`.
    let header = 0x7000;
    let status = 0x9000;

    write_u32_le(&mut mem, header, VIRTIO_BLK_T_FLUSH).unwrap();
    write_u32_le(&mut mem, header + 4, 0).unwrap();
    write_u64_le(&mut mem, header + 8, 0).unwrap();
    mem.write(status, &[0xff]).unwrap();

    write_desc(&mut mem, DESC_TABLE, 0, header, 16, 0x0001, 1);
    write_desc(&mut mem, DESC_TABLE, 1, status, 1, 0x0002, 0);

    // Initialise rings (flags/idx).
    write_u16_le(&mut mem, AVAIL_RING, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 1).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 4, 0).unwrap();
    write_u16_le(&mut mem, USED_RING, 0).unwrap();
    write_u16_le(&mut mem, USED_RING + 2, 0).unwrap();

    kick_queue0(&mut dev, &caps, &mut mem);

    assert_eq!(mem.get_slice(status, 1).unwrap()[0], 0);
    assert_eq!(flushes.load(Ordering::SeqCst), 1);

    // Function Mask should suppress MSI-X delivery without falling back to INTx. The pending bit
    // should latch in the PBA instead.
    assert_eq!(irq0.legacy_count(), 0);
    assert!(!irq0.legacy_level());
    assert!(irq0.take_msix_messages().is_empty());

    let pending = bar_read_u64(&mut dev, pba_offset);
    assert_eq!(
        pending & (1 << 1),
        1 << 1,
        "vector 1 should be pending in PBA"
    );

    // Clear the virtio interrupt cause (ISR is read-to-clear). Pending MSI-X delivery should still
    // occur once Function Mask is cleared, even without a new interrupt edge.
    let mut _isr = [0u8; 1];
    dev.bar0_read(caps.isr, &mut _isr);
    assert_eq!(
        bar_read_u64(&mut dev, pba_offset) & (1 << 1),
        1 << 1,
        "PBA pending bit should remain set after clearing the ISR"
    );

    // Clear the MSI-X Function Mask bit (bit 14). This must immediately deliver the pending vector
    // without requiring additional queue work.
    dev.config_write(msix_cap_offset + 0x02, &(msg_ctl | (1 << 15)).to_le_bytes());
    assert_eq!(
        flushes.load(Ordering::SeqCst),
        1,
        "unmask must not require a new completion"
    );
    assert_eq!(
        irq0.take_msix_messages(),
        vec![MsiMessage {
            address: 0xfee0_0000,
            data: 0x0045,
        }]
    );
    assert_eq!(
        bar_read_u64(&mut dev, pba_offset) & (1 << 1),
        0,
        "vector 1 pending bit must be cleared"
    );
}

#[test]
fn virtio_blk_snapshot_restore_preserves_msix_table_and_interrupt_delivery() {
    let irq0 = TestIrq::default();
    let (mut dev, caps, mem, backing, flushes, _irq0) = setup_with_irq(irq0);

    // Find MSI-X capability in PCI config space.
    let mut cfg = [0u8; 256];
    dev.config_read(0, &mut cfg);
    let mut ptr = cfg[0x34] as usize;
    let mut msix_cap_offset = None;
    while ptr != 0 {
        if cfg[ptr] == 0x11 {
            msix_cap_offset = Some(ptr as u16);
            break;
        }
        ptr = cfg[ptr + 1] as usize;
    }
    let msix_cap_offset = msix_cap_offset.expect("missing MSI-X capability");

    // Enable MSI-X.
    let msg_ctl = u16::from_le_bytes([
        cfg[msix_cap_offset as usize + 0x02],
        cfg[msix_cap_offset as usize + 0x03],
    ]);
    dev.config_write(msix_cap_offset + 0x02, &(msg_ctl | (1 << 15)).to_le_bytes());

    // MSI-X Table register: BIR + offset.
    let table = u32::from_le_bytes(
        cfg[msix_cap_offset as usize + 0x04..msix_cap_offset as usize + 0x08]
            .try_into()
            .unwrap(),
    );
    assert_eq!(table & 0x7, 0, "MSI-X table must live in BAR0");
    let table_offset = (table & !0x7) as u64;

    // Program table entry 1 (queue 0 vector: index 1 because index 0 is the config vector).
    let entry = table_offset + 16;
    bar_write_u32(&mut dev, entry, 0xfee0_0000);
    bar_write_u32(&mut dev, entry + 0x04, 0);
    bar_write_u32(&mut dev, entry + 0x08, 0x0045);
    bar_write_u32(&mut dev, entry + 0x0c, 0); // unmasked

    // Assign the queue MSI-X vector.
    bar_write_u16(&mut dev, caps.common + 0x16, 0); // queue_select
    bar_write_u16(&mut dev, caps.common + 0x1a, 1); // queue_msix_vector

    // Snapshot the device while MSI-X is enabled and the MSI-X table has been programmed.
    let snap_bytes = dev.save_state();
    let mem_snap = mem.clone();

    // Restore into a fresh device instance with the same disk backend and guest memory image.
    let irq1 = TestIrq::default();
    let backend = SharedDisk {
        data: backing.clone(),
        flushes: flushes.clone(),
    };
    let blk = VirtioBlk::new(Box::new(backend));
    let mut restored = VirtioPciDevice::new(Box::new(blk), Box::new(irq1.clone()));
    restored.load_state(&snap_bytes).unwrap();

    assert!(
        irq1.take_msix_messages().is_empty(),
        "load_state() must not trigger MSI messages"
    );
    assert_eq!(irq1.legacy_count(), 0);
    assert!(!irq1.legacy_level());

    let mut mem2 = mem_snap.clone();
    let caps_restored = parse_caps(&mut restored);

    // Sanity check: MSI-X table entry can be read back post-restore.
    let mut cfg2 = [0u8; 256];
    restored.config_read(0, &mut cfg2);
    let mut ptr = cfg2[0x34] as usize;
    let mut msix_cap_offset2 = None;
    while ptr != 0 {
        if cfg2[ptr] == 0x11 {
            msix_cap_offset2 = Some(ptr as u16);
            break;
        }
        ptr = cfg2[ptr + 1] as usize;
    }
    let msix_cap_offset2 = msix_cap_offset2.expect("missing MSI-X capability after restore");
    let table2 = u32::from_le_bytes(
        cfg2[msix_cap_offset2 as usize + 0x04..msix_cap_offset2 as usize + 0x08]
            .try_into()
            .unwrap(),
    );
    let table_offset2 = (table2 & !0x7) as u64;
    let entry2 = table_offset2 + 16;
    assert_eq!(bar_read_u32(&mut restored, entry2), 0xfee0_0000);
    assert_eq!(bar_read_u32(&mut restored, entry2 + 0x04), 0);
    assert_eq!(bar_read_u32(&mut restored, entry2 + 0x08), 0x0045);
    assert_eq!(bar_read_u32(&mut restored, entry2 + 0x0c), 0);

    // Build a FLUSH request so completion is observable via `flushes`.
    let header = 0x7000;
    let status = 0x9000;

    write_u32_le(&mut mem2, header, VIRTIO_BLK_T_FLUSH).unwrap();
    write_u32_le(&mut mem2, header + 4, 0).unwrap();
    write_u64_le(&mut mem2, header + 8, 0).unwrap();
    mem2.write(status, &[0xff]).unwrap();

    write_desc(&mut mem2, DESC_TABLE, 0, header, 16, 0x0001, 1);
    write_desc(&mut mem2, DESC_TABLE, 1, status, 1, 0x0002, 0);

    // Initialise rings (flags/idx).
    write_u16_le(&mut mem2, AVAIL_RING, 0).unwrap();
    write_u16_le(&mut mem2, AVAIL_RING + 2, 1).unwrap();
    write_u16_le(&mut mem2, AVAIL_RING + 4, 0).unwrap();
    write_u16_le(&mut mem2, USED_RING, 0).unwrap();
    write_u16_le(&mut mem2, USED_RING + 2, 0).unwrap();

    kick_queue0(&mut restored, &caps_restored, &mut mem2);
    assert_eq!(mem2.get_slice(status, 1).unwrap()[0], 0);
    assert_eq!(flushes.load(Ordering::SeqCst), 1);

    // Queue completion interrupt should be delivered via MSI-X with the exact programmed message,
    // not INTx.
    assert_eq!(irq1.legacy_count(), 0);
    assert!(!irq1.legacy_level());
    assert_eq!(
        irq1.take_msix_messages(),
        vec![MsiMessage {
            address: 0xfee0_0000,
            data: 0x0045,
        }]
    );
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
    assert_eq!(flushes.load(Ordering::SeqCst), 1);
    assert_eq!(dev.debug_queue_used_idx(&mem, 0), Some(1));
    assert_eq!(dev.debug_queue_progress(0), Some((1, 1, false)));
    assert_eq!(irq0.legacy_count(), 1);
    assert!(
        irq0.legacy_level(),
        "legacy irq should be asserted after flush completion"
    );

    // Snapshot the device + guest memory image after completion.
    let snap_bytes = dev.save_state();
    let mem_snap = mem.clone();

    // Restore into a fresh device instance (same disk backend, same guest memory image).
    let irq1 = TestIrq::default();
    let backend = SharedDisk {
        data: backing.clone(),
        flushes: flushes.clone(),
    };
    let blk = VirtioBlk::new(Box::new(backend));
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
    assert_eq!(
        flushes.load(Ordering::SeqCst),
        1,
        "duplicate FLUSH should not be executed after restore"
    );
    assert_eq!(restored.debug_queue_used_idx(&mem2, 0), Some(1));
    assert_eq!(irq1.legacy_count(), irq_before);

    // Add another FLUSH request and ensure it still completes post-restore.
    mem2.write(status, &[0xff]).unwrap();
    write_u16_le(&mut mem2, AVAIL_RING + 4 + 2, 0).unwrap(); // ring[1] = head 0
    write_u16_le(&mut mem2, AVAIL_RING + 2, 2).unwrap(); // idx = 2

    kick_queue0(&mut restored, &caps_restored, &mut mem2);
    assert_eq!(mem2.get_slice(status, 1).unwrap()[0], 0);
    assert_eq!(flushes.load(Ordering::SeqCst), 2);
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
    assert_eq!(flushes.load(Ordering::SeqCst), 0);
    assert_eq!(dev.debug_queue_used_idx(&mem, 0), Some(0));

    let snap_bytes = dev.save_state();
    let mem_snap = mem.clone();

    // Restore into a fresh device instance with the same disk backend and guest memory image.
    let irq1 = TestIrq::default();
    let backend = SharedDisk {
        data: backing.clone(),
        flushes: flushes.clone(),
    };
    let blk = VirtioBlk::new(Box::new(backend));
    let mut restored = VirtioPciDevice::new(Box::new(blk), Box::new(irq1));
    restored.load_state(&snap_bytes).unwrap();

    let mut mem2 = mem_snap.clone();

    // The platform should not need to re-notify after restore: the device can detect that
    // `avail.idx != next_avail` and process the pending entry.
    restored.process_notified_queues(&mut mem2);

    assert_eq!(mem2.get_slice(status, 1).unwrap()[0], 0);
    assert_eq!(flushes.load(Ordering::SeqCst), 1);
    assert_eq!(restored.debug_queue_used_idx(&mem2, 0), Some(1));
    assert_eq!(restored.debug_queue_progress(0), Some((1, 1, false)));
}

#[test]
fn virtio_pci_modern_dma_is_gated_on_pci_bus_master_enable() {
    let irq = TestIrq::default();
    let (mut dev, caps, mut mem, _backing, _flushes, irq) = setup_with_irq(irq);

    // Disable bus mastering after the driver has set up the virtqueue. The guest should still be
    // able to interact with BAR0 MMIO, but the transport must not touch guest memory until BME is
    // re-enabled.
    // Keep memory decoding enabled so BAR0 notify writes still reach the device, but disable bus
    // mastering so DMA is blocked.
    dev.config_write(0x04, &0x0002u16.to_le_bytes());

    // Build a FLUSH request.
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

    // Notify and attempt to process while BME is disabled.
    dev.bar0_write(caps.notify, &0u16.to_le_bytes());
    dev.process_notified_queues(&mut mem);

    assert_eq!(mem.get_slice(status, 1).unwrap()[0], 0xff);
    assert_eq!(dev.debug_queue_used_idx(&mem, 0), Some(0));
    assert_eq!(irq.legacy_count(), 0);
    assert!(!irq.legacy_level());

    // Re-enable bus mastering and ensure the pending notify is processed.
    dev.config_write(0x04, &0x0006u16.to_le_bytes());
    dev.process_notified_queues(&mut mem);

    assert_eq!(mem.get_slice(status, 1).unwrap()[0], 0);
    assert_eq!(dev.debug_queue_used_idx(&mem, 0), Some(1));
    assert_eq!(irq.legacy_count(), 1);
    assert!(irq.legacy_level());
}

#[test]
fn virtio_pci_modern_dma_is_gated_on_pci_bus_master_enable_in_process_notified_queues_bounded() {
    let irq = TestIrq::default();
    let (mut dev, caps, mut mem, _backing, _flushes, irq) = setup_with_irq(irq);

    // Disable bus mastering after the driver has set up the virtqueue. Keep memory decoding enabled
    // so BAR0 notify writes still reach the device, but block DMA.
    dev.config_write(0x04, &0x0002u16.to_le_bytes());

    // Build a FLUSH request.
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

    // Notify and attempt to process while BME is disabled.
    dev.bar0_write(caps.notify, &0u16.to_le_bytes());
    dev.process_notified_queues_bounded(&mut mem, 8);

    assert_eq!(mem.get_slice(status, 1).unwrap()[0], 0xff);
    assert_eq!(dev.debug_queue_used_idx(&mem, 0), Some(0));
    assert_eq!(irq.legacy_count(), 0);
    assert!(!irq.legacy_level());

    // Re-enable bus mastering and ensure the pending notify is processed.
    dev.config_write(0x04, &0x0006u16.to_le_bytes());
    dev.process_notified_queues_bounded(&mut mem, 8);

    assert_eq!(mem.get_slice(status, 1).unwrap()[0], 0);
    assert_eq!(dev.debug_queue_used_idx(&mem, 0), Some(1));
    assert_eq!(irq.legacy_count(), 1);
    assert!(irq.legacy_level());
}

#[test]
fn virtio_pci_modern_dma_is_gated_on_pci_bus_master_enable_in_poll_bounded() {
    let irq = TestIrq::default();
    let (mut dev, _caps, mut mem, _backing, _flushes, irq) = setup_with_irq(irq);

    // Disable bus mastering after the driver has set up the virtqueue. Polling must not touch
    // guest memory until BME is re-enabled.
    dev.config_write(0x04, &0x0002u16.to_le_bytes());

    // Build a FLUSH request.
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

    dev.poll_bounded(&mut mem, 8);

    assert_eq!(mem.get_slice(status, 1).unwrap()[0], 0xff);
    assert_eq!(dev.debug_queue_used_idx(&mem, 0), Some(0));
    assert_eq!(irq.legacy_count(), 0);
    assert!(!irq.legacy_level());

    dev.config_write(0x04, &0x0006u16.to_le_bytes());
    dev.poll_bounded(&mut mem, 8);

    assert_eq!(mem.get_slice(status, 1).unwrap()[0], 0);
    assert_eq!(dev.debug_queue_used_idx(&mem, 0), Some(1));
    assert_eq!(irq.legacy_count(), 1);
    assert!(irq.legacy_level());
}

#[test]
fn virtio_pci_modern_intx_disable_suppresses_line_but_retains_pending() {
    let irq = TestIrq::default();
    let (mut dev, caps, mut mem, _backing, _flushes, irq) = setup_with_irq(irq);

    // Enable bus mastering but disable INTx delivery.
    dev.config_write(0x04, &0x0406u16.to_le_bytes());

    // Build a FLUSH request.
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

    dev.bar0_write(caps.notify, &0u16.to_le_bytes());
    dev.process_notified_queues(&mut mem);

    // Request should complete, but INTx should be suppressed while disabled.
    assert_eq!(mem.get_slice(status, 1).unwrap()[0], 0);
    assert_eq!(dev.debug_queue_used_idx(&mem, 0), Some(1));
    assert_eq!(irq.legacy_count(), 0);
    assert!(!irq.legacy_level());
    assert!(!dev.irq_level());

    // Re-enable INTx and ensure the pending latch reasserts the line without additional work.
    dev.config_write(0x04, &0x0006u16.to_le_bytes());
    assert_eq!(irq.legacy_count(), 1);
    assert!(irq.legacy_level());
    assert!(dev.irq_level());
}

#[test]
fn virtio_pci_modern_msix_enable_suppresses_intx_line_but_retains_pending() {
    let irq = TestIrq::default();
    let (mut dev, caps, mut mem, _backing, _flushes, irq) = setup_with_irq(irq);

    // Build a FLUSH request to produce a legacy interrupt (INTx) and leave it pending.
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
    assert_eq!(dev.debug_queue_used_idx(&mem, 0), Some(1));
    assert_eq!(irq.legacy_count(), 1);
    assert!(irq.legacy_level());
    assert!(dev.irq_level());

    // Find MSI-X capability in PCI config space.
    let mut cfg = [0u8; 256];
    dev.config_read(0, &mut cfg);
    let mut ptr = cfg[0x34] as usize;
    let mut msix_cap_offset = None;
    while ptr != 0 {
        if cfg[ptr] == 0x11 {
            msix_cap_offset = Some(ptr as u16);
            break;
        }
        ptr = cfg[ptr + 1] as usize;
    }
    let msix_cap_offset = msix_cap_offset.expect("missing MSI-X capability");

    // Enable MSI-X; this should suppress legacy INTx assertion but preserve the internal pending
    // latch so disabling MSI-X later reasserts.
    let msg_ctl = u16::from_le_bytes([
        cfg[msix_cap_offset as usize + 0x02],
        cfg[msix_cap_offset as usize + 0x03],
    ]);
    dev.config_write(msix_cap_offset + 0x02, &(msg_ctl | (1 << 15)).to_le_bytes());
    assert!(!irq.legacy_level());
    assert!(!dev.irq_level());
    assert_eq!(irq.legacy_count(), 1);

    // Disable MSI-X again; the pending legacy interrupt should reassert INTx without additional
    // queue processing.
    dev.config_write(
        msix_cap_offset + 0x02,
        &(msg_ctl & !(1 << 15)).to_le_bytes(),
    );
    assert!(irq.legacy_level());
    assert!(dev.irq_level());
    assert_eq!(irq.legacy_count(), 2);
}

#[test]
fn virtio_pci_snapshot_preserves_pending_intx_while_intx_disable_set() {
    // This test exercises a subtle interaction:
    // - the device has a pending legacy interrupt (internal latch set)
    // - the guest has disabled INTx delivery via `PCI COMMAND.INTX_DISABLE`
    // - we snapshot/restore while delivery is disabled
    //
    // The pending latch must be preserved so the interrupt is delivered when INTx is re-enabled
    // after restore.
    let irq0 = TestIrq::default();
    let (mut dev, caps, mut mem, backing, flushes, irq0) = setup_with_irq(irq0);

    // Complete a request to set the legacy interrupt pending latch.
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

    dev.bar0_write(caps.notify, &0u16.to_le_bytes());
    dev.process_notified_queues(&mut mem);

    assert_eq!(mem.get_slice(status, 1).unwrap()[0], 0);
    assert_eq!(flushes.load(Ordering::SeqCst), 1);
    assert_eq!(dev.debug_queue_used_idx(&mem, 0), Some(1));
    assert_eq!(irq0.legacy_count(), 1);
    assert!(irq0.legacy_level());

    // Disable INTx delivery but *do not* read ISR (so the internal pending latch remains set).
    dev.config_write(0x04, &0x0406u16.to_le_bytes()); // MEM + BME + INTX_DISABLE
    assert!(
        !irq0.legacy_level(),
        "INTx line should be deasserted when disabled"
    );
    assert!(!dev.irq_level());

    // Snapshot while INTX_DISABLE is set.
    let snap_bytes = dev.save_state();
    let mem_snap = mem.clone();

    // Restore into a fresh device instance (same disk backend, same guest memory image).
    let irq1 = TestIrq::default();
    let backend = SharedDisk {
        data: backing.clone(),
        flushes: flushes.clone(),
    };
    let blk = VirtioBlk::new(Box::new(backend));
    let mut restored = VirtioPciDevice::new(Box::new(blk), Box::new(irq1.clone()));
    restored.load_state(&snap_bytes).unwrap();
    let mem2 = mem_snap.clone();

    // INTX_DISABLE should still suppress the external line after restore, but the pending latch
    // must be preserved.
    assert_eq!(restored.debug_queue_used_idx(&mem2, 0), Some(1));
    assert_eq!(flushes.load(Ordering::SeqCst), 1);
    assert_eq!(irq1.legacy_count(), 0);
    assert!(!irq1.legacy_level());
    assert!(!restored.irq_level());

    // Re-enable INTx and confirm the pending interrupt is delivered without additional queue work.
    restored.config_write(0x04, &0x0006u16.to_le_bytes()); // MEM + BME
    assert_eq!(irq1.legacy_count(), 1);
    assert!(irq1.legacy_level());
    assert!(restored.irq_level());
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
    mem.write(data, &vec![0xa5u8; SECTOR_SIZE_BYTES]).unwrap();
    mem.write(status, &[0xff]).unwrap();

    write_desc(&mut mem, DESC_TABLE, 0, header, 16, 0x0001, 1);
    write_desc(
        &mut mem,
        DESC_TABLE,
        1,
        data,
        SECTOR_SIZE_U32,
        0x0001 | 0x0002,
        2,
    );
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
fn invalid_status_descriptor_is_consumed_without_touching_disk() {
    let (mut dev, caps, mut mem, backing, _flushes) = setup();

    // Well-formed OUT request, but the status descriptor is invalid (not write-only). The device
    // should still consume the chain and advance the used ring without performing any disk I/O.
    let header = 0x7000;
    let data = 0x8000;
    let status = 0x9000;

    write_u32_le(&mut mem, header, VIRTIO_BLK_T_OUT).unwrap();
    write_u32_le(&mut mem, header + 4, 0).unwrap();
    write_u64_le(&mut mem, header + 8, 0).unwrap();
    mem.write(data, &vec![0xa5u8; SECTOR_SIZE_BYTES]).unwrap();
    mem.write(status, &[0xff]).unwrap();

    write_desc(&mut mem, DESC_TABLE, 0, header, 16, 0x0001, 1);
    write_desc(&mut mem, DESC_TABLE, 1, data, SECTOR_SIZE_U32, 0x0001, 2);
    // Invalid status descriptor: read-only.
    write_desc(&mut mem, DESC_TABLE, 2, status, 1, 0x0000, 0);

    write_u16_le(&mut mem, AVAIL_RING, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 1).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 4, 0).unwrap();
    write_u16_le(&mut mem, USED_RING, 0).unwrap();
    write_u16_le(&mut mem, USED_RING + 2, 0).unwrap();

    kick_queue0(&mut dev, &caps, &mut mem);

    assert_eq!(dev.debug_queue_used_idx(&mem, 0), Some(1));
    assert_eq!(mem.get_slice(status, 1).unwrap()[0], 0xff);
    assert!(backing.lock().unwrap().iter().all(|b| *b == 0));
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
    assert!(backing.lock().unwrap().iter().all(|b| *b == 0));
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

    mem.write(data, &[0u8; SECTOR_SIZE_BYTES]).unwrap();
    mem.write(status, &[0xff]).unwrap();

    write_desc(&mut mem, DESC_TABLE, 0, header, 16, 0x0001, 1);
    write_desc(
        &mut mem,
        DESC_TABLE,
        1,
        data,
        SECTOR_SIZE_U32,
        0x0001 | 0x0002,
        2,
    );
    write_desc(&mut mem, DESC_TABLE, 2, status, 1, 0x0002, 0);

    write_u16_le(&mut mem, AVAIL_RING, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 1).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 4, 0).unwrap();
    write_u16_le(&mut mem, USED_RING, 0).unwrap();
    write_u16_le(&mut mem, USED_RING + 2, 0).unwrap();

    kick_queue0(&mut dev, &caps, &mut mem);

    assert_eq!(mem.get_slice(status, 1).unwrap()[0], VIRTIO_BLK_S_IOERR);
    assert!(backing.lock().unwrap().iter().all(|b| *b == 0));
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

    let payload: Vec<u8> = (0..(SECTOR_SIZE_BYTES as u16))
        .flat_map(|v| v.to_le_bytes())
        .collect();
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
fn virtio_blk_disk_errors_surface_as_ioerr() {
    struct ErrorDisk {
        read_err: fn() -> StorageDiskError,
        write_err: fn() -> StorageDiskError,
        flush_err: fn() -> StorageDiskError,
    }

    impl VirtualDisk for ErrorDisk {
        fn capacity_bytes(&self) -> u64 {
            VIRTIO_BLK_SECTOR_SIZE
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

    fn setup_with_disk(disk: Box<dyn VirtualDisk>) -> (VirtioPciDevice, Caps, GuestRam) {
        let blk = VirtioBlk::new(disk);
        let dev = VirtioPciDevice::new(Box::new(blk), Box::new(InterruptLog::default()));
        setup_pci_device(dev)
    }

    let disk: Box<dyn VirtualDisk> = Box::new(ErrorDisk {
        read_err: || StorageDiskError::Unsupported("read"),
        write_err: || StorageDiskError::Unsupported("write"),
        flush_err: || StorageDiskError::CorruptImage("flush"),
    });

    let (mut dev, caps, mut mem) = setup_with_disk(disk);

    // Initialise rings.
    write_u16_le(&mut mem, AVAIL_RING, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 0).unwrap();
    write_u16_le(&mut mem, USED_RING, 0).unwrap();
    write_u16_le(&mut mem, USED_RING + 2, 0).unwrap();

    let header = 0x7000;
    let data = 0x8000;
    let status = 0x9000;

    // READ request should return IOERR if the disk returns an error.
    write_u32_le(&mut mem, header, VIRTIO_BLK_T_IN).unwrap();
    write_u32_le(&mut mem, header + 4, 0).unwrap();
    write_u64_le(&mut mem, header + 8, 0).unwrap();
    mem.write(data, &[0u8; SECTOR_SIZE_BYTES]).unwrap();
    mem.write(status, &[0xff]).unwrap();
    write_desc(&mut mem, DESC_TABLE, 0, header, 16, 0x0001, 1);
    write_desc(
        &mut mem,
        DESC_TABLE,
        1,
        data,
        SECTOR_SIZE_U32,
        0x0001 | 0x0002,
        2,
    );
    write_desc(&mut mem, DESC_TABLE, 2, status, 1, 0x0002, 0);
    write_u16_le(&mut mem, AVAIL_RING + 4, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 1).unwrap();
    kick_queue0(&mut dev, &caps, &mut mem);
    assert_eq!(mem.get_slice(status, 1).unwrap()[0], VIRTIO_BLK_S_IOERR);

    // WRITE request should return IOERR if the disk returns an error.
    write_u32_le(&mut mem, header, VIRTIO_BLK_T_OUT).unwrap();
    write_u32_le(&mut mem, header + 4, 0).unwrap();
    write_u64_le(&mut mem, header + 8, 0).unwrap();
    mem.write(data, &[0xa5u8; SECTOR_SIZE_BYTES]).unwrap();
    mem.write(status, &[0xff]).unwrap();
    write_desc(&mut mem, DESC_TABLE, 0, header, 16, 0x0001, 1);
    write_desc(&mut mem, DESC_TABLE, 1, data, SECTOR_SIZE_U32, 0x0001, 2);
    write_desc(&mut mem, DESC_TABLE, 2, status, 1, 0x0002, 0);
    write_u16_le(&mut mem, AVAIL_RING + 4 + 2, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 2).unwrap();
    kick_queue0(&mut dev, &caps, &mut mem);
    assert_eq!(mem.get_slice(status, 1).unwrap()[0], VIRTIO_BLK_S_IOERR);

    // FLUSH request should return IOERR if the disk returns an error.
    write_u32_le(&mut mem, header, VIRTIO_BLK_T_FLUSH).unwrap();
    write_u32_le(&mut mem, header + 4, 0).unwrap();
    write_u64_le(&mut mem, header + 8, 0).unwrap();
    mem.write(status, &[0xff]).unwrap();
    write_desc(&mut mem, DESC_TABLE, 0, header, 16, 0x0001, 1);
    write_desc(&mut mem, DESC_TABLE, 1, status, 1, 0x0002, 0);
    write_u16_le(&mut mem, AVAIL_RING + 4 + 4, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 3).unwrap();
    kick_queue0(&mut dev, &caps, &mut mem);
    assert_eq!(mem.get_slice(status, 1).unwrap()[0], VIRTIO_BLK_S_IOERR);
}

#[test]
fn virtio_blk_disk_errors_surface_as_ioerr_for_browser_storage_failures() {
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
            VIRTIO_BLK_SECTOR_SIZE
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
        (
            backend_unavailable,
            backend_unavailable,
            backend_unavailable,
        ),
        (not_supported, not_supported, not_supported),
        (io_error, io_error, io_error),
    ];

    for (read_err, write_err, flush_err) in disks {
        let backend: Box<dyn VirtualDisk> = Box::new(ErrorDisk {
            read_err: *read_err,
            write_err: *write_err,
            flush_err: *flush_err,
        });

        let blk = VirtioBlk::new(backend);
        let dev = VirtioPciDevice::new(Box::new(blk), Box::new(InterruptLog::default()));
        let (mut dev, caps, mut mem) = setup_pci_device(dev);

        // Initialise rings.
        write_u16_le(&mut mem, AVAIL_RING, 0).unwrap();
        write_u16_le(&mut mem, AVAIL_RING + 2, 0).unwrap();
        write_u16_le(&mut mem, USED_RING, 0).unwrap();
        write_u16_le(&mut mem, USED_RING + 2, 0).unwrap();

        // A FLUSH request should return IOERR for these disk failures.
        let header = 0x7000;
        let status = 0x9000;
        write_u32_le(&mut mem, header, VIRTIO_BLK_T_FLUSH).unwrap();
        write_u32_le(&mut mem, header + 4, 0).unwrap();
        write_u64_le(&mut mem, header + 8, 0).unwrap();
        mem.write(status, &[0xff]).unwrap();

        write_desc(&mut mem, DESC_TABLE, 0, header, 16, 0x0001, 1);
        write_desc(&mut mem, DESC_TABLE, 1, status, 1, 0x0002, 0);

        write_u16_le(&mut mem, AVAIL_RING + 4, 0).unwrap();
        write_u16_le(&mut mem, AVAIL_RING + 2, 1).unwrap();
        kick_queue0(&mut dev, &caps, &mut mem);

        assert_eq!(mem.get_slice(status, 1).unwrap()[0], VIRTIO_BLK_S_IOERR);
    }
}

#[test]
fn virtio_blk_rejects_excessive_descriptor_count_without_panicking() {
    let (mut dev, caps, mut mem, backing, _flushes) = setup();

    let header_base = 0x1000;
    let data_base = 0x2000;
    let status = 0x3000;
    let indirect = 0x7000;

    // OUT request at sector 0.
    write_u32_le(&mut mem, header_base, VIRTIO_BLK_T_OUT).unwrap();
    write_u32_le(&mut mem, header_base + 4, 0).unwrap();
    write_u64_le(&mut mem, header_base + 8, 0).unwrap();
    mem.write(status, &[0xff]).unwrap();

    // Build an indirect chain that is otherwise valid (aligned and within capacity), but exceeds
    // the per-request descriptor limit. Use many 1-byte data segments to emulate a hostile guest
    // forcing worst-case per-descriptor overhead.
    let header_descs = 16usize; // 16-byte request header split into 1-byte descriptors.
    let status_descs = 1usize;
    let mut data_descs = VIRTIO_BLK_MAX_REQUEST_DESCRIPTORS
        .saturating_add(1)
        .saturating_sub(header_descs + status_descs);
    let sector_size = VIRTIO_BLK_SECTOR_SIZE as usize;
    let rem = data_descs % sector_size;
    if rem != 0 {
        data_descs = data_descs.saturating_add(sector_size - rem);
    }
    let total_descs = header_descs + data_descs + status_descs;
    assert!(
        total_descs > VIRTIO_BLK_MAX_REQUEST_DESCRIPTORS,
        "test should exceed descriptor limit"
    );

    // Non-zero payload so we'd observe a write if the request were incorrectly processed.
    mem.get_slice_mut(data_base, data_descs).unwrap().fill(0xa5);

    // Indirect table entries: header bytes.
    for i in 0..header_descs {
        let flags = VIRTQ_DESC_F_NEXT;
        write_desc(
            &mut mem,
            indirect,
            i as u16,
            header_base + i as u64,
            1,
            flags,
            (i + 1) as u16,
        );
    }

    // Indirect table entries: data bytes (1-byte segments).
    for i in 0..data_descs {
        let idx = header_descs + i;
        let flags = VIRTQ_DESC_F_NEXT; // OUT data is read-only.
        write_desc(
            &mut mem,
            indirect,
            idx as u16,
            data_base + i as u64,
            1,
            flags,
            (idx + 1) as u16,
        );
    }

    // Status descriptor.
    let status_idx = total_descs - 1;
    write_desc(
        &mut mem,
        indirect,
        status_idx as u16,
        status,
        1,
        VIRTQ_DESC_F_WRITE,
        0,
    );

    // Main descriptor table: single indirect descriptor at index 0.
    let indirect_len = u32::try_from(total_descs * 16).unwrap();
    write_desc(
        &mut mem,
        DESC_TABLE,
        0,
        indirect,
        indirect_len,
        VIRTQ_DESC_F_INDIRECT,
        0,
    );

    write_u16_le(&mut mem, AVAIL_RING, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 1).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 4, 0).unwrap();
    write_u16_le(&mut mem, USED_RING, 0).unwrap();
    write_u16_le(&mut mem, USED_RING + 2, 0).unwrap();

    kick_queue0(&mut dev, &caps, &mut mem);

    assert_eq!(mem.get_slice(status, 1).unwrap()[0], VIRTIO_BLK_S_IOERR);
    assert_eq!(dev.debug_queue_used_idx(&mem, 0), Some(1));
    assert!(backing.lock().unwrap().iter().all(|b| *b == 0));
}

#[test]
fn virtio_blk_rejects_excessive_total_data_bytes() {
    let total_len = VIRTIO_BLK_MAX_REQUEST_DATA_BYTES + VIRTIO_BLK_SECTOR_SIZE;
    let total_len_u32 = u32::try_from(total_len).unwrap();
    let disk_len = usize::try_from(total_len).unwrap();
    let mem_len = usize::try_from(0x10000u64 + total_len + 0x1000).unwrap();
    let (mut dev, caps, mut mem, backing, _flushes) = setup_with_sizes(disk_len, mem_len);

    let header = 0x7000;
    let data = 0x10000;
    let status = 0x8000;

    write_u32_le(&mut mem, header, VIRTIO_BLK_T_OUT).unwrap();
    write_u32_le(&mut mem, header + 4, 0).unwrap();
    write_u64_le(&mut mem, header + 8, 0).unwrap();

    // Non-zero payload so we'd observe a write if the request were incorrectly processed.
    mem.write(data, &[0xa5u8; SECTOR_SIZE_BYTES]).unwrap();
    mem.write(status, &[0xff]).unwrap();

    write_desc(&mut mem, DESC_TABLE, 0, header, 16, VIRTQ_DESC_F_NEXT, 1);
    write_desc(
        &mut mem,
        DESC_TABLE,
        1,
        data,
        total_len_u32,
        VIRTQ_DESC_F_NEXT,
        2,
    );
    write_desc(&mut mem, DESC_TABLE, 2, status, 1, VIRTQ_DESC_F_WRITE, 0);

    write_u16_le(&mut mem, AVAIL_RING, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 1).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 4, 0).unwrap();
    write_u16_le(&mut mem, USED_RING, 0).unwrap();
    write_u16_le(&mut mem, USED_RING + 2, 0).unwrap();

    kick_queue0(&mut dev, &caps, &mut mem);

    assert_eq!(mem.get_slice(status, 1).unwrap()[0], VIRTIO_BLK_S_IOERR);
    assert_eq!(dev.debug_queue_used_idx(&mem, 0), Some(1));
    assert!(backing.lock().unwrap()[..SECTOR_SIZE_BYTES]
        .iter()
        .all(|b| *b == 0));
}

#[test]
fn virtio_blk_rejects_sector_offset_overflow() {
    let (mut dev, caps, mut mem, backing, _flushes) = setup();

    let header = 0x7000;
    let data = 0x8000;
    let status = 0x9000;

    // Choose a sector number that overflows `sector * 512`:
    // (u64::MAX / 512 + 1) * 512 == 2^64.
    let sector = u64::MAX / VIRTIO_BLK_SECTOR_SIZE + 1;
    write_u32_le(&mut mem, header, VIRTIO_BLK_T_OUT).unwrap();
    write_u32_le(&mut mem, header + 4, 0).unwrap();
    write_u64_le(&mut mem, header + 8, sector).unwrap();

    mem.write(data, &[0xa5u8; SECTOR_SIZE_BYTES]).unwrap();
    mem.write(status, &[0xff]).unwrap();

    write_desc(&mut mem, DESC_TABLE, 0, header, 16, VIRTQ_DESC_F_NEXT, 1);
    write_desc(
        &mut mem,
        DESC_TABLE,
        1,
        data,
        SECTOR_SIZE_U32,
        VIRTQ_DESC_F_NEXT,
        2,
    );
    write_desc(&mut mem, DESC_TABLE, 2, status, 1, VIRTQ_DESC_F_WRITE, 0);

    write_u16_le(&mut mem, AVAIL_RING, 0).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 2, 1).unwrap();
    write_u16_le(&mut mem, AVAIL_RING + 4, 0).unwrap();
    write_u16_le(&mut mem, USED_RING, 0).unwrap();
    write_u16_le(&mut mem, USED_RING + 2, 0).unwrap();

    kick_queue0(&mut dev, &caps, &mut mem);

    assert_eq!(mem.get_slice(status, 1).unwrap()[0], VIRTIO_BLK_S_IOERR);
    assert_eq!(dev.debug_queue_used_idx(&mem, 0), Some(1));
    assert!(backing.lock().unwrap().iter().all(|b| *b == 0));
}
